"""通过桌面端使用的子进程协议，核对 Rust 图片转换结果与旧引擎。"""
import os
import json
import subprocess
import struct
from io import BytesIO
import tempfile
import unittest
from pathlib import Path

from PIL import Image, ImageOps, ImageCms
import pypdfium2 as pdfium

ROOT = Path(__file__).resolve().parents[1]

EXE = Path(os.environ.get("BANKFLOW_TEST_WORKER", ROOT / "src-tauri/target/debug/bankflow-workbench.exe"))


class NativeImageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name)
        self.source = self.folder / "透明.png"
        Image.new("RGBA", (100, 50), (255, 0, 0, 0)).save(self.source)

    def request(self, **options):
        req = dict(input=str(self.source), password="", mode="image_convert",
                   output_dir=str(self.folder), format="png", width=40)
        req.update(options)
        return req

    def native(self, req):
        with subprocess.Popen([EXE, "--image-worker"], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, encoding="utf-8") as process:
            process.stdin.write(json.dumps(req) + "\n")
            process.stdin.flush()
            events = [json.loads(line) for line in process.stdout]
        self.assertEqual(process.returncode, 0)
        return events

    def test_fits_and_spider_gray_depth_and_orientation(self):
        header = "".join(f"{key:8}= {value:>20}".ljust(80)
                         for key, value in (("SIMPLE", "T"), ("BITPIX", 16), ("NAXIS", 2),
                                            ("NAXIS1", 3), ("NAXIS2", 2)))
        header = (header + "END".ljust(80)).ljust(2880).encode("ascii")
        source = self.folder / "样本.fits"
        source.write_bytes(header + struct.pack(">6H", 5, 200, 400, 65000, 17, 999))
        event = self.native(self.request(input=str(source), width=0))[-1]
        self.assertEqual(event["kind"], "completed", event)
        with Image.open(event["output"]) as result:
            self.assertEqual(list(result.get_flattened_data()), [65000, 17, 999, 5, 200, 400])

        for endian in ("<", ">"):
            with self.subTest(endian=endian):
                fields = [0.0] * 27
                for index, value in ((1, 1), (2, 2), (5, 1), (12, 3), (13, 90), (22, 1080), (23, 12)):
                    fields[index - 1] = value
                source = self.folder / "样本.spider"
                source.write_bytes(struct.pack(endian + "27f", *fields).ljust(1080, b"\0")
                                   + struct.pack(endian + "6f", -1.4, 1.8, 765, 23001, 99999, 42))
                event = self.native(self.request(input=str(source), width=0))[-1]
                self.assertEqual(event["kind"], "completed", event)
                with Image.open(event["output"]) as result:
                    self.assertEqual(list(result.get_flattened_data()), [0, 1, 765, 23001, 65535, 42])

    def test_legacy_raster_decoders(self):
        pixels = Image.frombytes("RGB", (3, 2), bytes(range(18)))
        samples = {}
        pcx = BytesIO()
        pixels.save(pcx, format="PCX")
        directory = [4100] + [0] * 1023
        samples["single.dcx"] = struct.pack("<1025I", 0x3ADE68B1, *directory) + pcx.getvalue()

        pixar = bytearray(1024)
        pixar[:4] = b"\x80\xe8\0\0"
        struct.pack_into("<2H", pixar, 416, pixels.height, pixels.width)
        struct.pack_into("<2H", pixar, 424, 14, 2)
        samples["rgb.pxr"] = pixar + pixels.tobytes()

        for version in (1, 2):
            for channels, mode in ((1, "L"), (4, "RGBA")):
                name = b"brush\0"
                record = b"" if version == 1 else struct.pack(">4sI", b"GIMP", 20)
                header = struct.pack(">5I", 20 + len(record + name), version, 3, 2, channels)
                samples[f"v{version}-{channels}.gbr"] = header + record + name + pixels.convert(mode).tobytes()

        mono = Image.new("1", (13, 3))
        mono.paste(255, (2, 1, 10, 2))
        plain = BytesIO()
        mono.save(plain, format="MSP")
        samples["plain.msp"] = plain.getvalue()
        from functools import reduce
        from operator import xor

        header = bytearray(32)
        struct.pack_into("<4sHH", header, 0, b"LinS", 13, 3)
        struct.pack_into("<H", header, 30, reduce(xor, struct.unpack("<16H", header)))
        samples["packed.msp"] = header + struct.pack("<3H", 0, 3, 3) + bytes((0, 2, 0x55, 2, 0x99, 0x88))

        for name, data in samples.items():
            with self.subTest(name=name):
                source = self.folder / name
                source.write_bytes(data)
                event = self.native(self.request(input=str(source), width=0))[-1]
                self.assertEqual(event["kind"], "completed", event)
                with Image.open(source) as expected, Image.open(event["output"]) as actual:
                    self.assertEqual(expected.size, actual.size)
                    self.assertEqual(expected.convert("RGBA").tobytes(), actual.convert("RGBA").tobytes())

        directory[1] = directory[0] + len(pcx.getvalue())
        source = self.folder / "multiple.dcx"
        source.write_bytes(struct.pack("<1025I", 0x3ADE68B1, *directory) + pcx.getvalue() * 2)
        event = self.native(self.request(input=str(source), width=0))[-1]
        self.assertEqual(event["kind"], "error")
        self.assertIn("多帧", event["message"])

    def test_formats_match_original(self):
        for fmt in ("png", "jpg", "webp", "bmp", "tiff"):
            with self.subTest(fmt=fmt):
                req = self.request(format=fmt)
                events = self.native(req)
                self.assertEqual(events[0], dict(kind="progress", page=1, total=1))
                self.assertEqual(events[-1]["kind"], "completed")
                with Image.open(events[-1]["output"]) as new:
                    self.assertEqual(new.size, (40, 20))
                    if fmt in ("png", "webp", "tiff"):
                        self.assertEqual(new.convert("RGBA").getchannel("A").getextrema(), (0, 0))
                    if fmt in ("jpg", "bmp"):
                        self.assertEqual(new.convert("RGB").getpixel((0, 0)), (255, 255, 255))

    def test_image_pdf_roundtrip(self):
        first = Image.new("RGBA", (100, 50), (240, 0, 0, 255))
        first.paste((0, 0, 255, 255), (0, 25, 100, 50))
        first.save(self.source)
        second = self.folder / "透明背景.png"
        Image.new("RGBA", (60, 80), (0, 0, 0, 0)).save(second)
        req = self.request(mode="images_pdf", inputs=[str(self.source), str(second)], width=0)
        merged = self.native(req)
        self.assertEqual([e["kind"] for e in merged], ["progress", "progress", "completed"])
        output = Path(merged[-1]["output"])
        with pdfium.PdfDocument(output) as doc:
            self.assertEqual(len(doc), 2)
            self.assertEqual(doc.get_page_size(0), (50, 25))
            self.assertEqual(doc.get_page_size(1), (30, 40))
        result = self.native(self.request(input=str(output), mode="pdf_images", width=0))
        self.assertEqual([e["kind"] for e in result], ["progress", "progress", "completed"])
        pages = Path(result[-1]["output"])
        with Image.open(pages / "001.png") as page:
            self.assertEqual(page.size, (100, 50))
            self.assertGreater(page.getpixel((10, 5))[0], 225)
            self.assertGreater(page.getpixel((10, 45))[2], 240)
        with Image.open(pages / "002.png") as page:
            self.assertEqual(page.size, (60, 80))
            self.assertEqual(page.getpixel((10, 10)), (255, 255, 255))

    def test_extra_codecs_match_source(self):
        picture = Image.new("RGB", (43, 59), (0, 80, 200))
        picture.paste((240, 10, 0), (23, 20, 40, 58))
        for ext in ("qoi", "jp2", "pcx", "dib", "sgi"):
            with self.subTest(ext=ext):
                source = self.folder / ("样本." + ext)
                picture.save(source)
                event = self.native(self.request(input=str(source), width=0))[-1]
                self.assertEqual(event["kind"], "completed")
                with Image.open(event["output"]) as converted, Image.open(source) as original:
                    self.assertEqual(converted.convert("RGB").tobytes(), original.convert("RGB").tobytes())

    def test_avif_channels_orientation_and_frames(self):
        import random
        pixels = random.Random(73).randbytes(31 * 47 * 4)
        rgba = Image.frombytes("RGBA", (31, 47), pixels)
        source = self.folder / "透明颜色.avif"
        for mode in ("L", "RGB", "RGBA"):
            for i, subsampling in enumerate(("4:0:0", "4:2:0", "4:2:2", "4:4:4")):
                exif = Image.Exif()
                exif[274] = (1, 2, 5, 6)[i]
                rgba.convert(mode).save(source, subsampling=subsampling, exif=exif, speed=10)
                with self.subTest(mode=mode, sampling=subsampling):
                    event = self.native(self.request(input=str(source), width=0))[-1]
                    self.assertEqual(event["kind"], "completed", event)
                    with Image.open(BytesIO(source.read_bytes())) as old, Image.open(event["output"]) as new:
                        expected = ImageOps.exif_transpose(old)
                        self.assertEqual(expected.size, new.size)
                        self.assertEqual(expected.convert("RGBA").tobytes(), new.convert("RGBA").tobytes())
        profile = ImageCms.ImageCmsProfile(ImageCms.createProfile("sRGB")).tobytes()
        rgba.save(source, icc_profile=profile, speed=10)
        for fmt in ("png", "tiff"):
            event = self.native(self.request(input=str(source), width=0, format=fmt))[-1]
            with Image.open(event["output"]) as converted:
                self.assertEqual(converted.info.get("icc_profile"), profile)
        rgba.save(source, save_all=True, append_images=[rgba], speed=10)
        before = set(self.folder.iterdir())
        for mode in ("image_convert", "images_pdf"):
            event = self.native(self.request(mode=mode, input=str(source), width=0))[-1]
            self.assertEqual(event["kind"], "error")
            self.assertIn("多帧", event["message"])
        self.assertEqual(before, set(self.folder.iterdir()))

    def test_psd_composite(self):
        source = self.folder / "合成图.psd"
        width, height = 5, 9
        profile = ImageCms.ImageCmsProfile(ImageCms.createProfile("sRGB")).tobytes()
        resource = b"8BIM" + struct.pack(">H2xI", 1039, len(profile)) + profile
        for channels in (1, 3, 4):
            for compressed in (False, True):
                with self.subTest(channels=channels, compressed=compressed):
                    head = struct.pack(">4sH6xHIIHH", b"8BPS", 1, channels, height, width,
                                       8, 1 if channels == 1 else 3)
                    sections = struct.pack(">II", 0, len(resource)) + resource + bytes(4)
                    rows = []
                    for plane in range(channels):
                        for y in range(height):
                            sample = bytes((19 * plane + y, 27 + y, 41 + y, 61 + y, 61 + y))
                            rows.append(bytes((128, 2)) + sample[:3] + bytes((255, sample[3]))
                                        if compressed else sample)
                    counts = b"".join(struct.pack(">H", len(r)) for r in rows) if compressed else b""
                    source.write_bytes(head + sections + struct.pack(">H", int(compressed))
                                       + counts + b"".join(rows))
                    event = self.native(self.request(input=str(source), width=0))[-1]
                    self.assertEqual(event["kind"], "completed", event)
                    with Image.open(BytesIO(source.read_bytes())) as old, Image.open(event["output"]) as new:
                        self.assertEqual(new.info.get("icc_profile"), profile)
                        self.assertEqual(old.convert("RGBA").tobytes(), new.convert("RGBA").tobytes())
        event = self.native(self.request(mode="images_pdf", inputs=[str(source), str(self.source)],
                                         width=0))[-1]
        self.assertEqual(event["kind"], "completed", event)
        with pdfium.PdfDocument(event["output"]) as doc:
            self.assertEqual(len(doc), 2)
            self.assertEqual(doc.get_page_size(0), (width / 2, height / 2))
        # 混合批次保留 PSD 合成图与 XBM 位图的像素。
        xbm = self.folder / "位图.xbm"
        Image.new("1", (10, 17)).save(xbm)
        req = self.request(inputs=[str(source), str(xbm)], width=0)
        actual = self.native(req)[-1]
        self.assertEqual(actual["kind"], "completed", actual)
        for name, original in (("001.png", source), ("002.png", xbm)):
            with Image.open(Path(actual["output"]) / name) as new, Image.open(BytesIO(original.read_bytes())) as old:
                self.assertEqual(new.convert("RGBA").tobytes(), old.convert("RGBA").tobytes())
        data = source.read_bytes()
        for bad in (data[:-3], data[:14] + bytes.fromhex("7fffffff") + data[18:],
                    data[:4] + bytes((0, 2)) + data[6:]):
            source.write_bytes(bad)
            before = set(self.folder.iterdir())
            event = self.native(self.request(input=str(source), width=0))[-1]
            self.assertEqual(event["kind"], "error", event)
            self.assertEqual(before, set(self.folder.iterdir()))

    def test_mixed_native_and_im_decode(self):
        source = self.folder / "旧图像.im"
        data = BytesIO()
        Image.new("RGB", (7, 3), (3, 70, 211)).save(data, format="IM")
        source.write_bytes(data.getvalue())
        inputs = [str(self.source), str(source)]
        completed = self.native(self.request(inputs=inputs, width=0))[-1]
        self.assertEqual(completed["kind"], "completed", completed)
        with Image.open(Path(completed["output"]) / "001.png") as first:
            self.assertEqual(first.getpixel((0, 0)), (255, 0, 0, 0))
        with Image.open(Path(completed["output"]) / "002.png") as second:
            self.assertEqual(second.convert("RGB").getpixel((0, 0)), (3, 70, 211))
        merged = self.native(self.request(mode="images_pdf", inputs=inputs, width=0))[-1]
        self.assertEqual(merged["kind"], "completed", merged)
        with pdfium.PdfDocument(merged["output"]) as doc:
            self.assertEqual(len(doc), 2)
            self.assertEqual(doc.get_page_size(1), (3.5, 1.5))

    def test_gif_frame_count(self):
        source = self.folder / "静态.gif"
        frame = Image.new("RGB", (20, 30), "red")
        frame.save(source)
        self.assertEqual(self.native(self.request(input=str(source)))[-1]["kind"], "completed")
        frame.save(source, save_all=True, append_images=[Image.new("RGB", frame.size, "blue")])
        self.assertEqual(self.native(self.request(input=str(source), mode="images_pdf"))[-1]["kind"], "error")

    def test_batch_rollback_and_multiframe(self):
        req = self.request(inputs=[str(self.source)] * 2)
        output = Path(self.native(req)[-1]["output"])
        self.assertEqual(sorted(p.name for p in output.iterdir()), ["001.png", "002.png"])
        pages = self.folder / "多页.tiff"
        page = Image.new("RGB", (20, 10))
        page.save(pages, save_all=True, append_images=[page])
        before = set(self.folder.iterdir())
        req["inputs"][1] = str(pages)
        self.assertEqual(self.native(req)[-1]["kind"], "error")
        self.assertEqual(before, set(self.folder.iterdir()))


if __name__ == "__main__":
    unittest.main()
