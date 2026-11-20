#!/usr/bin/env python3
"""Extract UE1 v79 retail textures and upscale them with 4x-UltraSharp (RRDBNet).

Generates external HD PNG overrides for hp2-wgpu's HDTextures directory without
modifying copyrighted retail .utx/.u packages.

Key quality features:
- Circular wrap-padding on tiling world textures so castle walls/floors have zero seams.
- Foreground RGB dilation/inpainting into palette-index-0 pixels before ESRGAN on
  masked textures (foliage, fences, UI icons) to eliminate dark/magenta alpha fringes.
- Crisp cubic alpha upscaling for AlphaToCoverage foliage/cutouts.
- Output capped at 1024x1024 max (4x on <=256px textures) for fast loading and lean VRAM.
"""

from __future__ import annotations

import argparse
import os
import struct
import sys
import time
from collections import defaultdict
from pathlib import Path

import cv2
import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F
from PIL import Image

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "Build"))
import package79_reference as p79  # noqa: E402


# ---- Minimal pure-PyTorch ESRGAN (RRDBNet old-arch, matches 4x-UltraSharp.pth) ----

class ResidualDenseBlock(nn.Module):
    def __init__(self, nf: int = 64, gc: int = 32) -> None:
        super().__init__()
        self.conv1 = nn.Sequential(nn.Conv2d(nf, gc, 3, 1, 1))
        self.conv2 = nn.Sequential(nn.Conv2d(nf + gc, gc, 3, 1, 1))
        self.conv3 = nn.Sequential(nn.Conv2d(nf + 2 * gc, gc, 3, 1, 1))
        self.conv4 = nn.Sequential(nn.Conv2d(nf + 3 * gc, gc, 3, 1, 1))
        self.conv5 = nn.Sequential(nn.Conv2d(nf + 4 * gc, nf, 3, 1, 1))
        self.lrelu = nn.LeakyReLU(0.2, inplace=True)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        x1 = self.lrelu(self.conv1(x))
        x2 = self.lrelu(self.conv2(torch.cat((x, x1), 1)))
        x3 = self.lrelu(self.conv3(torch.cat((x, x1, x2), 1)))
        x4 = self.lrelu(self.conv4(torch.cat((x, x1, x2, x3), 1)))
        x5 = self.conv5(torch.cat((x, x1, x2, x3, x4), 1))
        return x5 * 0.2 + x


class RRDB(nn.Module):
    def __init__(self, nf: int = 64, gc: int = 32) -> None:
        super().__init__()
        self.RDB1 = ResidualDenseBlock(nf, gc)
        self.RDB2 = ResidualDenseBlock(nf, gc)
        self.RDB3 = ResidualDenseBlock(nf, gc)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        out = self.RDB1(x)
        out = self.RDB2(out)
        out = self.RDB3(out)
        return out * 0.2 + x


class ShortcutBlock(nn.Module):
    def __init__(self, submodule: nn.Module) -> None:
        super().__init__()
        self.sub = submodule

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return x + self.sub(x)


class ESRGAN4x(nn.Module):
    def __init__(self, in_nc: int = 3, out_nc: int = 3, nf: int = 64, nb: int = 23, gc: int = 32) -> None:
        super().__init__()
        trunk = [RRDB(nf, gc) for _ in range(nb)]
        trunk.append(nn.Conv2d(nf, nf, 3, 1, 1))
        self.model = nn.Sequential(
            nn.Conv2d(in_nc, nf, 3, 1, 1),          # 0
            ShortcutBlock(nn.Sequential(*trunk)),   # 1
            nn.Upsample(scale_factor=2, mode="nearest"),  # 2
            nn.Conv2d(nf, nf, 3, 1, 1),             # 3
            nn.LeakyReLU(0.2, inplace=True),        # 4
            nn.Upsample(scale_factor=2, mode="nearest"),  # 5
            nn.Conv2d(nf, nf, 3, 1, 1),             # 6
            nn.LeakyReLU(0.2, inplace=True),        # 7
            nn.Conv2d(nf, nf, 3, 1, 1),             # 8
            nn.LeakyReLU(0.2, inplace=True),        # 9
            nn.Conv2d(nf, out_nc, 3, 1, 1),         # 10
        )

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return self.model(x)


def load_model(weights_path: Path, device: torch.device) -> nn.Module:
    sd = torch.load(weights_path, map_location="cpu", weights_only=True)
    if "params_ema" in sd:
        sd = sd["params_ema"]
    elif "params" in sd:
        sd = sd["params"]
    model = ESRGAN4x()
    model.load_state_dict(sd, strict=True)
    model.eval().to(device)
    if device.type == "cuda":
        model.half()
    return model


# ---- UE1 v79 UTexture & UPalette reader ----

def read_properties(data: bytes, offset: int, names) -> tuple[dict, int]:
    cur = p79.Cursor(data, "", offset, "props")
    props: dict = {}
    while True:
        idx = cur.compact_index()
        name = names[idx].text
        if name == "None":
            break
        info = cur.u8()
        kind = info & 0x0F
        size_code = info & 0x70
        array_flag = bool(info & 0x80)
        if kind == 10:
            cur.compact_index()
        if size_code == 0x00:
            size = 1
        elif size_code == 0x10:
            size = 2
        elif size_code == 0x20:
            size = 4
        elif size_code == 0x30:
            size = 12
        elif size_code == 0x40:
            size = 16
        elif size_code == 0x50:
            size = cur.u8()
        elif size_code == 0x60:
            size = cur.u16()
        else:
            size = cur.i32()
        if array_flag and kind != 3:
            b = cur.u8()
            if b >= 192:
                cur.take(3)
            elif b >= 128:
                cur.take(1)
        if kind == 3:
            val = array_flag
        else:
            raw = cur.take(size)
            if kind == 1:
                val = raw[0]
            elif kind == 2:
                val = struct.unpack("<i", raw)[0]
            elif kind == 4:
                val = struct.unpack("<f", raw)[0]
            elif kind == 5:
                val = p79.Cursor(raw, "", 0, "obj").compact_index()
            elif kind == 6:
                val = names[p79.Cursor(raw, "", 0, "name").compact_index()].text
            else:
                val = raw
        props[name] = val
    return props, cur.pos


def read_palette(data: bytes, exp, names) -> np.ndarray | None:
    if exp.serial_offset is None or exp.serial_size <= 0:
        return None
    _, pos = read_properties(data, exp.serial_offset, names)
    cur = p79.Cursor(data, "", pos, "pal")
    count = cur.compact_index()
    if count <= 0 or count > 256:
        return None
    raw = cur.take(count * 4)
    arr = np.frombuffer(raw, dtype=np.uint8).reshape(count, 4).copy()
    if count < 256:
        padded = np.zeros((256, 4), dtype=np.uint8)
        padded[:count] = arr
        arr = padded
    arr[:, 3] = 255
    return arr


NON_TILING_PREFIXES = (
    "menuart", "hp2_menu", "hp_menu", "hp_logo", "hp_titles", "hp_portraits",
    "3dwizardcards", "intro_cs", "hgame", "hpmodels", "hprops", "hpparticle",
    "uwindowfonts", "hpedit", "aoldemo",
)


def is_tiling_texture(pkg_lower: str, w: int, h: int, is_masked: bool) -> bool:
    if is_masked:
        return False
    if any(pkg_lower.startswith(p) for p in NON_TILING_PREFIXES):
        return False
    # Power-of-two world textures >= 32px generally tile across BSP surfaces.
    return w >= 32 and h >= 32 and (w & (w - 1) == 0) and (h & (h - 1) == 0)


def inpaint_masked_rgb(rgb: np.ndarray, opaque_mask: np.ndarray) -> np.ndarray:
    """Fill transparent (index-0) pixels with nearest foreground RGB to prevent ESRGAN halos."""
    if opaque_mask.all() or not opaque_mask.any():
        return rgb
    transparent = (~opaque_mask).astype(np.uint8)
    # Fast distance-transform nearest-neighbor color propagation
    _, labels = cv2.distanceTransformWithLabels(
        transparent, cv2.DIST_L2, 5, labelType=cv2.DIST_LABEL_PIXEL
    )
    fg_coords = np.argwhere(opaque_mask)
    # labels are 1-indexed into foreground pixels in row-major order
    out = rgb.copy()
    ty, tx = np.nonzero(~opaque_mask)
    lbl = labels[ty, tx] - 1
    valid = (lbl >= 0) & (lbl < len(fg_coords))
    src_y = fg_coords[lbl[valid], 0]
    src_x = fg_coords[lbl[valid], 1]
    out[ty[valid], tx[valid]] = rgb[src_y, src_x]
    return out


def collect_packages(retail_dir: Path, lang: str) -> list[tuple[Path, str]]:
    """Return list of (package_path, runtime_package_name_lower)."""
    pkgs: list[tuple[Path, str]] = []
    tex_dir = retail_dir / "Textures"
    if tex_dir.is_dir():
        localized_menu = None
        for p in sorted(tex_dir.iterdir()):
            if not p.is_file():
                continue
            low = p.name.lower()
            if low == f"menuart.{lang}_utx":
                localized_menu = p
                continue
            if not low.endswith(".utx"):
                continue
            if low in ("uwindowfonts.utx", "hpedit.utx", "palettes.utx"):
                continue
            if low == "menuart.utx":
                continue  # handled below so localized MenuArt takes precedence
            pkgs.append((p, p.stem.lower()))
        if localized_menu is not None:
            pkgs.append((localized_menu, "menuart"))
        elif (tex_dir / "MenuArt.utx").is_file():
            pkgs.append((tex_dir / "MenuArt.utx", "menuart"))

    sys_dir = retail_dir / "System"
    for name in ("HPModels.u", "HProps.u", "HGame.u", "HPParticle.u"):
        p = sys_dir / name
        if p.is_file():
            pkgs.append((p, p.stem.lower()))
    return pkgs


def extract_all_textures(retail_dir: Path, lang: str) -> list[dict]:
    items: list[dict] = []
    seen_keys: set[str] = set()
    for pkg_path, pkg_name in collect_packages(retail_dir, lang):
        data = pkg_path.read_bytes()
        if len(data) < 4 or struct.unpack_from("<I", data, 0)[0] != p79.PACKAGE_TAG:
            continue
        parsed = p79.PackageReader(pkg_path.name, data).parse()

        # Pre-decode palettes in this package (export ref is 1-based positive index)
        palettes: dict[int, np.ndarray] = {}
        for i, exp in enumerate(parsed.exports):
            cls = "Core.Class" if exp.class_ref == 0 else (parsed.resolver.ref_path(exp.class_ref) or "")
            if cls.rsplit(".", 1)[-1] == "Palette":
                pal = read_palette(data, exp, parsed.names)
                if pal is not None:
                    palettes[i + 1] = pal

        for i, exp in enumerate(parsed.exports):
            cls = "Core.Class" if exp.class_ref == 0 else (parsed.resolver.ref_path(exp.class_ref) or "")
            if cls.rsplit(".", 1)[-1] != "Texture" or exp.serial_offset is None:
                continue
            props, pos = read_properties(data, exp.serial_offset, parsed.names)
            if props.get("Format", 0) != 0:
                continue
            pal_ref = props.get("Palette", 0)
            pal = palettes.get(pal_ref)
            if pal is None:
                continue
            cur = p79.Cursor(data, "", pos, "mips")
            mip_cnt = cur.u8()
            if mip_cnt <= 0:
                continue
            _skip = cur.i32()
            nbytes = cur.compact_index()
            mip0 = cur.take(nbytes)
            w = cur.i32()
            h = cur.i32()
            if w < 16 or h < 16 or len(mip0) != w * h:
                continue

            # Replace package prefix with runtime package name (e.g. menuart.pol_utx -> menuart)
            raw_path = parsed.resolver.export_path(i)
            parts = raw_path.split(".")
            parts[0] = pkg_name
            full_key = ".".join(parts).lower()
            if full_key in seen_keys:
                continue
            seen_keys.add(full_key)

            indices = np.frombuffer(mip0, dtype=np.uint8).reshape(h, w)
            rgba = pal[indices].copy()
            is_masked = bool(props.get("bMasked", False))
            has_idx0 = bool((indices == 0).any())
            # Skip completely flat 1-color utility textures
            if np.all(indices == indices[0, 0]):
                continue
            items.append({
                "key": full_key,
                "pkg": pkg_name,
                "w": w,
                "h": h,
                "rgba": rgba,
                "indices": indices,
                "is_masked": is_masked,
                "has_idx0": has_idx0,
                "tileable": is_tiling_texture(pkg_name, w, h, is_masked),
            })
    return items


@torch.inference_mode()
def upscale_batch(
    model: nn.Module,
    device: torch.device,
    batch_items: list[dict],
    max_dim: int,
    out_dir: Path,
) -> int:
    """Upscale a batch of same-size textures with padding and save PNGs."""
    # Group inside by (tileable) so padding is uniform
    saved = 0
    for tileable_flag in (True, False):
        sub = [it for it in batch_items if it["tileable"] == tileable_flag]
        if not sub:
            continue
        w, h = sub[0]["w"], sub[0]["h"]
        pad = 16 if tileable_flag else 8
        pad = min(pad, w, h)
        border = cv2.BORDER_WRAP if tileable_flag else cv2.BORDER_REFLECT_101

        tensors = []
        alphas = []
        for it in sub:
            rgb = it["rgba"][:, :, :3]
            if it["is_masked"] and it["has_idx0"]:
                opaque = it["indices"] != 0
                rgb = inpaint_masked_rgb(rgb, opaque)
                alphas.append(opaque.astype(np.uint8) * 255)
            else:
                alphas.append(None)
            padded = cv2.copyMakeBorder(rgb, pad, pad, pad, pad, border)
            t = torch.from_numpy(padded).permute(2, 0, 1).float().div_(255.0)
            tensors.append(t)

        inp = torch.stack(tensors, dim=0).to(device, non_blocking=True)
        if device.type == "cuda":
            inp = inp.half()
        out = model(inp).float().clamp_(0.0, 1.0)
        # Crop 4x pad
        p4 = pad * 4
        out = out[:, :, p4 : p4 + h * 4, p4 : p4 + w * 4]
        out_np = (out.permute(0, 2, 3, 1).mul_(255.0).round_().byte()).cpu().numpy()

        for idx, it in enumerate(sub):
            up_rgb = out_np[idx]
            uh, uw = up_rgb.shape[:2]
            if max(uw, uh) > max_dim:
                scale = max_dim / max(uw, uh)
                nw, nh = max(1, int(round(uw * scale))), max(1, int(round(uh * scale)))
                up_rgb = cv2.resize(up_rgb, (nw, nh), interpolation=cv2.INTER_AREA)
                uw, uh = nw, nh

            alpha = alphas[idx]
            out_path = out_dir / f"{it['key']}.png"
            if alpha is not None:
                # Pad + cubic upscale alpha mask, then crisp threshold curve for clean AlphaToCoverage
                up_a = cv2.resize(alpha, (uw, uh), interpolation=cv2.INTER_CUBIC)
                # Slightly sharpen alpha transition while preserving smooth edge AA
                af = up_a.astype(np.float32) / 255.0
                af = np.clip((af - 0.5) * 1.6 + 0.5, 0.0, 1.0)
                up_a = (af * 255.0).round().astype(np.uint8)
                rgba_out = np.dstack([up_rgb, up_a])
                Image.fromarray(rgba_out, mode="RGBA").save(out_path, compress_level=4)
            else:
                Image.fromarray(up_rgb, mode="RGB").save(out_path, compress_level=4)
            saved += 1
    return saved


def main() -> None:
    parser = argparse.ArgumentParser(description="Extract and AI-upscale HP2 retail textures")
    parser.add_argument("--retail-dir", type=Path, required=True)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--lang", default="pol")
    parser.add_argument("--max-dim", type=int, default=1024)
    args = parser.parse_args()

    args.output.mkdir(parents=True, exist_ok=True)
    t0 = time.time()
    print(f"Extracting textures from {args.retail_dir} (lang={args.lang})...")
    items = extract_all_textures(args.retail_dir, args.lang.lower())
    print(f"Found {len(items)} unique P8 textures in {time.time() - t0:.1f}s")

    # Skip already-generated files so re-runs resume cleanly
    todo = [it for it in items if not (args.output / f"{it['key']}.png").exists()]
    print(f"Upscaling {len(todo)} textures (skipping {len(items) - len(todo)} existing)...")
    if not todo:
        return

    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    print(f"Loading {args.model.name} on {device}...")
    model = load_model(args.model, device)

    # Group by (w, h) for efficient CUDA batching without OOM on 6 GB VRAM
    buckets: dict[tuple[int, int], list[dict]] = defaultdict(list)
    for it in todo:
        buckets[(it["w"], it["h"])].append(it)

    done = 0
    t_start = time.time()
    for (w, h), group in sorted(buckets.items(), key=lambda kv: kv[0][0] * kv[0][1]):
        # Pick safe batch size for 6 GB VRAM (256x256 padded 288x288 -> 1152x1152 needs ~1.2GB/img)
        pixels = (w + 32) * (h + 32)
        if pixels <= 64 * 64:
            bs = 16
        elif pixels <= 144 * 144:
            bs = 8
        elif pixels <= 200 * 200:
            bs = 4
        else:
            bs = 2
        for i in range(0, len(group), bs):
            done += upscale_batch(model, device, group[i : i + bs], args.max_dim, args.output)
            if done % 100 == 0 or done == len(todo):
                elapsed = time.time() - t_start
                rate = done / max(elapsed, 0.1)
                eta = (len(todo) - done) / max(rate, 0.01)
                print(f"  [{done}/{len(todo)}] {rate:.1f} tex/s (ETA {eta:.0f}s) last={w}x{h}", flush=True)

    print(f"Done! {done} HD textures saved to {args.output} in {time.time() - t_start:.1f}s")


if __name__ == "__main__":
    main()
