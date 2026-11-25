"""验证桌面 Rust 图片入口的像素、色彩描述和错误语义。"""
import struct
import os
from io import BytesIO
from unittest.mock import patch
import tempfile
import unittest
from pathlib import Path

from PIL import Image, ImageCms, ImageOps, ImImagePlugin

from verification.smoke_worker import run_worker


def decode_to_png(source, dest):
    with patch.dict(os.environ, {"BANKFLOW_PYTHON": str(dest.parent / "不存在.exe")}):
        last = run_worker(dict(input=str(source), mode="image_convert", format="png", width=0))[-1]
    if last["kind"] != "completed":
        raise ValueError(last["message"])
    Path(last["output"]).replace(dest)


class ImageDecodeTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.folder = Path(temp.name)
        self.source = self.folder / "来源.bin"
        self.dest = self.folder / "解码.png"

    def test_palette_transparency_and_rgb_profile(self):
        palette = Image.new("P", (3, 1))
        palette.putpalette([255, 0, 0, 0, 255, 0, 0, 0, 255])
        palette.putdata([0, 1, 2])
        icc = ImageCms.ImageCmsProfile(ImageCms.createProfile("sRGB")).tobytes()
        palette.save(self.source, "PNG", transparency=bytes([0, 127, 255]), icc_profile=icc)
        decode_to_png(self.source, self.dest)
        with Image.open(self.dest) as decoded:
            self.assertEqual(decoded.info["icc_profile"], icc)
            self.assertEqual(decoded.convert("RGBA").tobytes(),
                             bytes([255, 0, 0, 0, 0, 255, 0, 127, 0, 0, 255, 255]))

    def test_orientation_is_applied_once(self):
        image = Image.new("RGB", (3, 2))
        image.putdata([(n * 35, n * 20, n * 10) for n in range(6)])
        exif = Image.Exif()
        exif[274] = 6
        image.save(self.source, "PNG", exif=exif)
        decode_to_png(self.source, self.dest)
        with Image.open(BytesIO(self.source.read_bytes())) as original, Image.open(self.dest) as decoded:
            self.assertEqual(decoded.size, (2, 3))
            self.assertEqual(decoded.tobytes(), ImageOps.exif_transpose(original).tobytes())
            self.assertNotIn(274, decoded.getexif())

    def test_numeric_grayscale_preserves_16_bit_range(self):
        for mode in ("I;16", "I;16B", "I", "F"):
            with self.subTest(mode=mode):
                image = Image.new(mode, (4, 1))
                for x, value in enumerate((0, 255, 16835, 65535)):
                    image.putpixel((x, 0), value)
                image.save(self.source, "TIFF")
                decode_to_png(self.source, self.dest)
                with Image.open(self.dest) as decoded:
                    self.assertEqual([decoded.getpixel((x, 0)) for x in range(4)],
                                     [0, 255, 16835, 65535])

    def test_changed_color_space_drops_source_icc(self):
        for mode, color in (("LAB", (128, 150, 90)), ("CMYK", (0, 70, 90, 15))):
            with self.subTest(mode=mode):
                image = Image.new(mode, (2, 1), color)
                image.save(self.source, "TIFF", icc_profile=b"source-color-profile")
                with Image.open(self.source) as original:
                    expected = original.convert("RGB").tobytes()
                decode_to_png(self.source, self.dest)
                with Image.open(self.dest) as decoded:
                    self.assertEqual(decoded.mode, "RGB")
                    if mode == "LAB":
                        self.assertTrue(all(abs(a-b) <= 2 for a,b in zip(decoded.tobytes(), expected)))
                        self.assertEqual(decoded.info["icc_profile"][16:20], b"RGB ")
                    else:
                        self.assertEqual(decoded.tobytes(), expected)
                        self.assertNotIn("icc_profile", decoded.info)

    def test_rejects_multiple_frames(self):
        first = Image.new("RGB", (2, 1), "red")
        second = Image.new("RGB", (2, 1), "blue")
        first.save(self.source, "GIF", save_all=True, append_images=[second])
        with self.assertRaisesRegex(ValueError, "多帧"):
            decode_to_png(self.source, self.dest)
        self.assertFalse(self.dest.exists())

    def test_psd_composite_resource(self):
        for valid in (True, False):
            with self.subTest(valid=valid):
                header = struct.pack(">4sH6xHIIHH", b"8BPS", 1, 3, 1, 2, 8, 3)
                version = struct.pack(">IB", 1, valid)
                resource = b"8BIM" + struct.pack(">H2xI", 1057, len(version)) + version + b"\0"
                sections = struct.pack(">II", 0, len(resource)) + resource + bytes(4)
                self.source.write_bytes(header + sections + bytes(2) + bytes([1, 2, 3, 4, 5, 6]))
                if valid:
                    decode_to_png(self.source, self.dest)
                    with Image.open(self.dest) as decoded:
                        self.assertEqual(decoded.tobytes(), bytes([1, 3, 5, 2, 4, 6]))
                else:
                    with self.assertRaisesRegex(ValueError, "真实合成图"):
                        decode_to_png(self.source, self.dest)


    def test_im_pixel_layouts(self):
        import random
        rng = random.Random(27)
        seen = 0
        for mode in ImImagePlugin.OPEN:
            for palette in [None, rng.randbytes(768)] if mode in ("Greyscale image", "LA image", "PA image", "B2 image", "B4 image") else [None]:
                with self.subTest(mode=mode, palette=palette is not None):
                    header = f"Image type: {mode}\nImage size (x*y): 13*4\n".encode()
                    if palette is not None:
                        header += b"Lut: 1\n"
                    raw = bytearray(rng.randbytes(4 * 13 * 4))
                    if mode.startswith("L*") and mode.split()[0][2:].isdigit():
                        bits = int(mode.split()[0][2:])
                        if 2 <= bits < 32 and bits not in (8,16) and 13*bits % 8:
                            row_size = (13*bits + 7) // 8
                            # 合法图像按扫描行补零，不将行末的填充解释为下一个像素。
                            for y in range(4):
                                raw[(y+1)*row_size-1] &= (1 << (13*bits % 8)) - 1
                    blob = header + b"\x1a" + (palette or b"") + raw
                    try:
                        with Image.open(BytesIO(blob)) as original:
                            original.load()
                            if original.mode == "F":
                                reference = original.convert("I").convert("I;16")
                            elif original.mode in ("I", "I;16L", "I;16B"):
                                reference = original.convert("I").convert("I;16")
                            elif original.mode == "P" and not original.getpalette():
                                # 无色表的 B2/B4 在旧 PNG 编码器中会报错。
                                continue
                            elif original.mode not in ("I;16",):
                                reference = original.convert("RGBA")
                            else:
                                reference = original.copy()
                    except ValueError as e:
                        if mode in ("RLB image","RYB image","PA image"):
                            continue
                        raise
                    self.source.write_bytes(blob)
                    decode_to_png(self.source, self.dest)
                    with Image.open(self.dest) as saved:
                        actual = saved.convert(reference.mode)
                        self.assertEqual(actual.tobytes(), reference.tobytes())
                    seen += 1
        self.assertGreater(seen, 55)

    def test_im_rejects_animation_and_truncation(self):
        for fields, data, message in [(b"File size (no of images): 2\n",bytes(12),"多帧"),
                                      (b"",bytes(2),"不完整")]:
            self.source.write_bytes(b"Image type: RGB image\nImage size (x*y): 2*1\n"+fields+b"\x1a"+data)
            with self.assertRaisesRegex(ValueError, message):
                decode_to_png(self.source,self.dest)

    def test_legacy_icns_with_alpha_and_embedded_images(self):
        def block(tag, data):
            return tag + struct.pack(">I", 8 + len(data)) + data

        size = 16
        count = size * size
        mask = bytes(range(count))
        # 单色 RLE 平面，每个 run 重复 128 次。
        rle = b"".join(bytes([253, v]) * 2 for v in (1, 144, 221))
        legacy = block(b"is32", rle) + block(b"s8mk", mask)
        embedded = Image.new("RGBA", (32, 32), (190, 17, 31, 234))
        for fmt in (None, "PNG", "JPEG2000"):
            with self.subTest(fmt=fmt):
                chunks = legacy
                if fmt:
                    data = BytesIO()
                    embedded.save(data, format=fmt)
                    chunks += block(b"icp5", data.getvalue())
                self.source.write_bytes(block(b"icns", chunks))
                decode_to_png(self.source, self.dest)
                with Image.open(BytesIO(self.source.read_bytes())) as original, Image.open(self.dest) as decoded:
                    self.assertEqual(original.convert("RGBA").tobytes(),decoded.convert("RGBA").tobytes())

    def test_text_rasters(self):
        raw = bytes([12, 45, 123, 67])
        for hdr in (b"*notes\nwidth 2\nheight 2\npixel n8\n\x0c", b"P7 332\n# comment\n2 2 255\n"):
            self.source.write_bytes(hdr + raw)
            decode_to_png(self.source,self.dest)
            with Image.open(BytesIO(self.source.read_bytes())) as original, Image.open(self.dest) as decoded:
                self.assertEqual(original.convert("RGB").tobytes(),decoded.convert("RGB").tobytes())

    def test_windows_metafiles(self):
        from functools import reduce
        from operator import xor

        def record(opcode, *values):
            return struct.pack("<IH", 3 + len(values), opcode) + struct.pack("<" + "h" * len(values), *values)

        records = [record(0x0103, 8), record(0x020B, 0, 0), record(0x020C, 55, 73),
                   record(0x0214, 4, 4), record(0x0213, 45, 63), record(0)]
        length = sum(len(r) for r in records) // 2
        header = struct.pack("<I H hhhh H I", 0x9ac6cdd7, 0, 0, 0, 73, 55, 72, 0)
        checksum = reduce(xor, struct.unpack("<10H",header))
        body = struct.pack("<HHHIHIH",1,9,0x300,9+length,1,max(map(len,records))//2,0)
        self.source.write_bytes(header + struct.pack("<H",checksum) + body + b"".join(records))
        decode_to_png(self.source,self.dest)
        with Image.open(BytesIO(self.source.read_bytes())) as original, Image.open(self.dest) as decoded:
            self.assertEqual(decoded.size,(73,55))
            self.assertGreater(len(decoded.getcolors()),1)
            self.assertEqual(original.convert("RGB").tobytes(),decoded.tobytes())
        self.source.write_bytes(self.source.read_bytes()[:32])
        with self.assertRaises(ValueError):
            decode_to_png(self.source,self.dest)
