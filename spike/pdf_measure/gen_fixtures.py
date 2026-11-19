#!/usr/bin/env python3
"""Generate synthetic PDFs for pdf_measure.

No personal documents. Fixed seeds, so a given reportlab/Pillow pair should
produce the same bytes aside from encryption salts inside the encrypted file.

Install:
    python3 -m pip install reportlab pillow

Tested with reportlab 5.1 and Pillow 13. Encryption is PDF revision 3
(128-bit), which does not need the optional pyaes package.
"""

from __future__ import annotations

import argparse
import io
import random
import re
import sys
from pathlib import Path

from PIL import Image, ImageDraw
from reportlab import rl_config
from reportlab.lib.pagesizes import letter
from reportlab.lib.utils import ImageReader
from reportlab.pdfgen.canvas import Canvas
from reportlab.lib.pdfencrypt import StandardEncryption

USER_PASSWORD = "spacepeek"
OWNER_PASSWORD = "spacepeek-owner"
SECRET_MARKER = "UNENCRYPTED_MARKER_spacepeek_secret"
PAGE_RE = re.compile(rb"/Type\s*/Page(?!s)")
TEXT_PAGES = 320

PARAGRAPHS = (
    "The preview window asks only for the first page, fitted inside a box, with the aspect ratio left alone.",
    "This paragraph is synthetic filler so a long document still has ordinary text to parse and paint.",
    "Headings, body lines, and repeated sentences are enough. No scanned handwriting and no real correspondence.",
    "A measurement spike can load hundreds of pages and still rasterize page one.",
    "Keep the wording dull and deterministic. The clock cares about the work, not the prose.",
    "Windows.Data.Pdf will open the stream, count the pages, and render page zero into a bitmap.",
)


def main() -> int:
    parser = argparse.ArgumentParser(description="Write synthetic PDF fixtures.")
    parser.add_argument(
        "--out",
        type=Path,
        default=Path(__file__).resolve().parent / "fixtures",
        help="output directory (default: spike/pdf_measure/fixtures)",
    )
    args = parser.parse_args()
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    # Embed JPEG and content streams as binary filters, not ASCII85.
    rl_config.useA85 = 0

    small = out / "small_text.pdf"
    long_doc = out / "text_300pp.pdf"
    scan = out / "scan_image.pdf"
    vector = out / "vector_heavy.pdf"
    encrypted = out / "encrypted.pdf"
    broken = out / "broken.pdf"

    print("writing small_text.pdf", flush=True)
    write_text_pdf(small, pages=1)
    print("writing text_300pp.pdf", flush=True)
    write_text_pdf(long_doc, pages=TEXT_PAGES)
    print("writing scan_image.pdf", flush=True)
    write_scan_pdf(scan)
    print("writing vector_heavy.pdf", flush=True)
    write_vector_pdf(vector)
    print("writing encrypted.pdf", flush=True)
    write_encrypted_pdf(encrypted)
    print("writing broken.pdf", flush=True)
    write_broken_pdf(small, broken)

    check(small, long_doc, scan, vector, encrypted, broken)
    print("fixtures:")
    total = 0
    for path in (small, long_doc, scan, vector, encrypted, broken):
        size = path.stat().st_size
        total += size
        print(f"  {path}  {size} bytes")
    print(f"total {total} bytes")
    return 0


def canvas(path: Path, pagesize, encrypt=None) -> Canvas:
    return Canvas(
        str(path),
        pagesize=pagesize,
        invariant=1,
        pageCompression=1,
        encrypt=encrypt,
    )


def write_text_pdf(path: Path, pages: int) -> None:
    width, height = letter
    pdf = canvas(path, letter)
    for page in range(pages):
        pdf.setFillColorRGB(0, 0, 0)
        pdf.setFont("Helvetica-Bold", 14)
        pdf.drawString(54, height - 54, f"Synthetic text page {page + 1} of {pages}")
        text = pdf.beginText(54, height - 80)
        text.setFont("Times-Roman", 11)
        text.setLeading(14)
        for line in range(42):
            paragraph = PARAGRAPHS[(page + line) % len(PARAGRAPHS)]
            text.textLine(f"{page + 1}.{line + 1}  {paragraph}")
        pdf.drawText(text)
        pdf.showPage()
    pdf.save()


def write_scan_pdf(path: Path) -> None:
    jpeg = make_scan_jpeg()
    # 3000x4000 px at 300 dpi -> 10 x 13.333 in -> 720 x 960 pt.
    page = (720, 960)
    pdf = canvas(path, page)
    pdf.drawImage(ImageReader(io.BytesIO(jpeg)), 0, 0, page[0], page[1], preserveAspectRatio=False)
    pdf.save()


def make_scan_jpeg() -> bytes:
    src_w, src_h = 600, 800
    rng = random.Random(1)
    raw = bytearray(src_w * src_h * 3)
    for y in range(src_h):
        gy = (230 * y) // (src_h - 1)
        row = y * src_w * 3
        for x in range(src_w):
            gx = (210 * x) // (src_w - 1)
            noise = rng.randrange(56)
            index = row + x * 3
            raw[index] = min(255, gx // 2 + noise + 20)
            raw[index + 1] = min(255, gy // 2 + (noise // 2) + 16)
            raw[index + 2] = min(255, (gx + gy) // 5 + noise // 3 + 24)
    resample = getattr(Image, "Resampling", Image).BICUBIC
    image = Image.frombytes("RGB", (src_w, src_h), bytes(raw))
    image = image.resize((3000, 4000), resample)
    draw = ImageDraw.Draw(image)
    draw.rectangle((48, 48, 2952, 360), outline=(20, 20, 20), width=8)
    draw.text((90, 120), "SYNTHETIC SCAN 3000x4000 seed=1", fill=(12, 12, 12))
    for band in range(18):
        top = 420 + band * 180
        draw.rectangle((90, top, 2800, top + 28), fill=(30, 30, 30))
        draw.rectangle((90, top + 48, 2200, top + 68), fill=(70, 70, 70))
    encoded = io.BytesIO()
    image.save(encoded, format="JPEG", quality=75, optimize=False, subsampling=2)
    return encoded.getvalue()


def write_vector_pdf(path: Path) -> None:
    width, height = letter
    pdf = canvas(path, letter)
    pdf.setStrokeColorRGB(0.75, 0.75, 0.78)
    pdf.setLineWidth(0.15)
    x = 18.0
    while x < width - 18:
        pdf.line(x, 18, x, height - 18)
        x += 4.0
    y = 18.0
    while y < height - 18:
        pdf.line(18, y, width - 18, y)
        y += 4.0

    rng = random.Random(1)
    pdf.setStrokeColorRGB(0.12, 0.28, 0.55)
    pdf.setLineWidth(0.35)
    for i in range(2500):
        px = 20 + (i * 17) % int(width - 40)
        py = 20 + (i * 29) % int(height - 40)
        path_obj = pdf.beginPath()
        path_obj.moveTo(px, py)
        for _ in range(6):
            px = min(width - 18, max(18, px + rng.randint(-28, 28)))
            py = min(height - 18, max(18, py + rng.randint(-28, 28)))
            path_obj.lineTo(px, py)
        pdf.drawPath(path_obj, stroke=1, fill=0)

    pdf.setStrokeColorRGB(0.55, 0.12, 0.12)
    pdf.setLineWidth(0.5)
    for i in range(600):
        x0 = 24 + (i * 13) % int(width - 80)
        y0 = 24 + (i * 19) % int(height - 80)
        pdf.bezier(x0, y0, x0 + 30, y0 + 70, x0 + 60, y0 - 25, x0 + 15, y0 + 40)

    pdf.setFillColorRGB(0, 0, 0)
    pdf.setFont("Helvetica", 9)
    pdf.drawString(24, height - 14, "synthetic vector-heavy page")
    pdf.showPage()
    pdf.save()


def write_encrypted_pdf(path: Path) -> None:
    encrypt = StandardEncryption(
        USER_PASSWORD,
        ownerPassword=OWNER_PASSWORD,
        canPrint=0,
        canModify=0,
        canCopy=0,
        canAnnotate=0,
        strength=128,
    )
    width, height = letter
    pdf = canvas(path, letter, encrypt=encrypt)
    pdf.setFont("Helvetica", 12)
    pdf.drawString(54, height - 72, SECRET_MARKER)
    pdf.drawString(54, height - 96, "Synthetic encrypted page. Open only with the known password.")
    pdf.showPage()
    pdf.save()


def write_broken_pdf(source: Path, dest: Path) -> None:
    raw = source.read_bytes()
    if not raw.startswith(b"%PDF"):
        raise SystemExit(f"{source} is not a PDF")
    cut = max(16, len(raw) // 2)
    dest.write_bytes(raw[:cut])


def check(
    small: Path,
    long_doc: Path,
    scan: Path,
    vector: Path,
    encrypted: Path,
    broken: Path,
) -> None:
    small_raw = small.read_bytes()
    long_raw = long_doc.read_bytes()
    scan_raw = scan.read_bytes()
    vector_raw = vector.read_bytes()
    encrypted_raw = encrypted.read_bytes()
    broken_raw = broken.read_bytes()

    if len(PAGE_RE.findall(small_raw)) < 1:
        raise SystemExit("small_text.pdf has no page object")
    long_pages = len(PAGE_RE.findall(long_raw))
    if long_pages < 300:
        raise SystemExit(f"text_300pp.pdf page objects = {long_pages}, want >= 300")
    if b"\xff\xd8\xff" not in scan_raw:
        raise SystemExit("scan_image.pdf does not contain a JPEG stream")
    if len(vector_raw) < 20_000:
        raise SystemExit(f"vector_heavy.pdf is unexpectedly small ({len(vector_raw)} bytes)")
    if b"/Encrypt" not in encrypted_raw:
        raise SystemExit("encrypted.pdf has no /Encrypt dictionary")
    if SECRET_MARKER.encode("ascii") in encrypted_raw:
        raise SystemExit("encrypted.pdf still contains the plaintext marker")
    if not broken_raw.startswith(b"%PDF") or len(broken_raw) >= len(small_raw):
        raise SystemExit("broken.pdf was not truncated")
    if b"startxref" in broken_raw and broken_raw.rstrip().endswith(b"%%EOF"):
        raise SystemExit("broken.pdf still looks complete")


if __name__ == "__main__":
    sys.exit(main())
