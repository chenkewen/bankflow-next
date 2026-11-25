"""验证桌面流水线的 PSD/PSB 合成像素、位深、压缩边界和色彩转换。"""
from io import BytesIO
import json
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
import zlib

import cv2
import numpy as np
from PIL import Image, ImageCms

EXE = Path(os.environ.get("BANKFLOW_TEST_WORKER", Path(__file__).resolve().parents[1] / "src-tauri/target/debug/bankflow-workbench.exe"))


def document(data, mode, depth, version, compression, *, palette=b"", resources=(), alpha=False,
             tagged=None):
    channels, height, width = data.shape
    header = struct.pack(">4sH6xHIIHH", b"8BPS", version, channels, height, width, depth, mode)
    sections = struct.pack(">I", len(palette)) + palette
    res = bytearray()
    for key, blob in resources:
        res.extend(b"8BIM" + struct.pack(">H2xI", key, len(blob)) + blob + bytes(len(blob) % 2))
    sections += struct.pack(">I", len(res)) + res
    length = ">Q" if version == 2 else ">I"
    # 负图层数标识第一个额外通道为合成透明度。
    layers = struct.pack(length, 2) + struct.pack(">h", -1) if alpha else b""
    if tagged:
        key, value = tagged
        layers = (struct.pack(length, 0) + bytes(4) + b"8BIM" + key
                  + struct.pack(length, len(value)) + value + bytes(-len(value) % 4))
    sections += struct.pack(length, len(layers)) + layers
    raw = []
    for plane in data:
        for row in plane:
            if depth == 1:
                scan = np.packbits(row).tobytes()
            else:
                scan = row.astype({8: "u1", 16: ">u2", 32: ">f4"}[depth]).tobytes()
            if compression == 3:
                if depth == 16:
                    values = np.frombuffer(scan, dtype=">u2").astype(np.int32)
                    scan = np.diff(values, prepend=0).astype(">u2").tobytes()
                else:
                    values = np.frombuffer(scan, dtype="u1").astype(np.int32)
                    if depth == 32:
                        values = values.reshape(width, 4).T.ravel()
                    scan = np.diff(values, prepend=0).astype("u1").tobytes()
            elif compression == 1:
                scan = b"".join(bytes([len(scan[k:k+128])-1]) + scan[k:k+128]
                                for k in range(0, len(scan), 128))
            raw.append(scan)
    encoded = b"".join(raw)
    if compression == 1:
        encoded = b"".join(struct.pack(">I" if version == 2 else ">H", len(s))
                           for s in raw) + encoded
    elif compression >= 2:
        encoded = zlib.compress(encoded)
    return header + sections + struct.pack(">H", compression) + encoded


class PhotoshopSmoke(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def convert(self, blob, fmt="png", expect_error=False):
        source = self.root / ("sample.psb" if blob[4:6] == b"\0\2" else "sample.psd")
        source.write_bytes(blob)
        with subprocess.Popen([EXE, "--image-worker"], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, encoding="utf-8") as p:
            p.stdin.write(json.dumps(dict(input=str(source), password="", mode="image_convert",
                                          output_dir=str(self.root), width=0, format=fmt)) + "\n")
            p.stdin.flush()
            event = json.loads(p.stdout.readlines()[-1])
        self.assertEqual(event["kind"], "error" if expect_error else "completed", event)
        if not expect_error:
            return Path(event["output"])

    def test_depth_compression_and_versions(self):
        rng = np.random.default_rng(47)
        for depth in (8, 16, 32):
            for channels in (1, 3, 4):
                data = (rng.integers(0, 1 << depth, (channels, 4, 17),
                                     dtype=np.uint8 if depth == 8 else np.uint16)
                        if depth < 32 else rng.uniform(-0.125, 2.25, (channels, 4, 17)).astype(np.float32))
                for version in (1, 2):
                    for compression in (0, 1, 2, 3):
                        with self.subTest(depth=depth, channels=channels, version=version, compression=compression):
                            blob = document(data, 1 if channels == 1 else 3, depth, version, compression)
                            out = self.convert(blob, "tiff" if depth == 32 else "png")
                            actual = cv2.imdecode(np.frombuffer(out.read_bytes(), "u1"), cv2.IMREAD_UNCHANGED)
                            expected = data.transpose(1, 2, 0)
                            if channels == 1 and depth == 32:
                                expected = np.repeat(expected, 3, axis=2)
                            elif channels == 1:
                                expected = expected[:, :, 0]
                            else:
                                expected = expected[:, :, [2, 1, 0] if channels == 3 else [2, 1, 0, 3]]
                            np.testing.assert_array_equal(actual, expected)

    def test_color_modes_and_profiles(self):
        rng = np.random.default_rng(23)
        cmyk = rng.integers(0, 256, (4, 4, 17), dtype="u1")
        index = cmyk[:1]
        palette = rng.integers(0, 256, 768, dtype="u1").tobytes()
        for data, mode, pal in [(cmyk, 4, b""), (index, 2, palette)]:
            blob = document(data, mode, 8, 1, 1, palette=pal)
            with Image.open(BytesIO(blob)) as reference, Image.open(self.convert(blob)) as actual:
                np.testing.assert_array_equal(actual.convert("RGB"), reference.convert("RGB"))
        transparent = document(index, 2, 8, 2, 2, palette=palette, resources=[(1047, struct.pack(">H", int(index[0, 1, 2])))])
        with Image.open(self.convert(transparent)) as actual:
            np.testing.assert_array_equal(np.asarray(actual)[:, :, 3], 255 * (index[0] != index[0, 1, 2]))
        for version in (1, 2):
            bits = index % 2
            blob = document(bits, 0, 1, version, 1)
            with Image.open(self.convert(blob)) as actual:
                np.testing.assert_array_equal(np.asarray(actual), (1 - bits[0]) * 255)
        gray_alpha = document(cmyk[:2], 1, 8, 2, 3, alpha=True)
        with Image.open(self.convert(gray_alpha)) as actual:
            np.testing.assert_array_equal(np.asarray(actual), cmyk[:2].transpose(1, 2, 0))
        profile = Path(os.environ.get("WINDIR", "C:/Windows")) / "System32/spool/drivers/color/RSWOP.icm"
        if profile.is_file():
            blob = document(cmyk, 4, 8, 1, 0, resources=[(1039, profile.read_bytes())])
            dest = ImageCms.createProfile("sRGB")
            with Image.open(BytesIO(blob)) as raw, Image.open(self.convert(blob)) as actual:
                ref = ImageCms.profileToProfile(raw, str(profile), dest, outputMode="RGB")
                difference = abs(np.asarray(ref).astype(np.int16) - np.asarray(actual).astype(np.int16))
                self.assertLessEqual(int(difference.max()), 5)
                self.assertIsNotNone(actual.info.get("icc_profile"))
        lab = np.array([[[20, 70, 127, 230]], [[128, 80, 130, 200]], [[128, 150, 185, 100]]], dtype="u1")
        blob = document(lab, 9, 8, 1, 0)
        with Image.open(BytesIO(blob)) as raw, Image.open(self.convert(blob)) as actual:
            ref = raw.convert("RGB")
            diff = abs(np.asarray(ref).astype(np.int16) - np.asarray(actual).astype(np.int16))
            self.assertLessEqual(int(diff.max()), 5)

    def test_gray_preview_and_transparency_tags(self):
        rng = np.random.default_rng(31)
        planes = rng.integers(0, 256, (3, 3, 9), dtype="u1")
        for mode in (7, 8):
            # 旧入口保留专色/双色调首个通道的灰度预览。
            blob = document(planes if mode == 7 else planes[:1], mode, 8, 1, 0)
            with Image.open(BytesIO(blob)) as raw, Image.open(self.convert(blob)) as actual:
                np.testing.assert_array_equal(actual, raw)
        planes16 = planes.astype("u2") * 257
        for version in (1, 2):
            for key, value in ((b"Lr16", b"\xff\xff"), (b"Mt16", b"")):
                blob = document(planes16, 1, 16, version, 3, tagged=(key, value))
                out = self.convert(blob)
                color = cv2.imdecode(np.frombuffer(out.read_bytes(), dtype="u1"), cv2.IMREAD_UNCHANGED)
                np.testing.assert_array_equal(color[:, :, 3], planes16[1])
                np.testing.assert_array_equal(color[:, :, 2], planes16[0])
        # 正图层数表示附加的保存通道，不是合成图透明度。
        rgb_spot = document(np.concatenate((planes, planes[:1])), 3, 8, 1, 0,
                            tagged=(b"Layr", b"\x00\x01"))
        with Image.open(self.convert(rgb_spot)) as actual:
            self.assertEqual(actual.mode, "RGB")
            np.testing.assert_array_equal(actual, planes.transpose(1, 2, 0))

    def test_float_to_png(self):
        levels = np.array([[-0.1, 0, 0.25, 0.5, 1.0, 1.5]], dtype="f4")
        for channels in (1, 3, 4):
            planes = np.repeat(levels[None, :, :], channels, axis=0)
            blob = document(planes, 1 if channels == 1 else 3, 32, 2, 3)
            out = self.convert(blob)
            actual = cv2.imdecode(np.frombuffer(out.read_bytes(), dtype="u1"), cv2.IMREAD_UNCHANGED)
            expected = np.rint(levels.clip(0, 1) * 65535).astype("u2")
            self.assertEqual(actual.dtype, np.uint16)
            np.testing.assert_array_equal(actual[:, :, 0], expected)
            if channels == 4:
                np.testing.assert_array_equal(actual[:, :, 3], expected)

    def test_damaged_archives(self):
        data = np.ones((3, 4, 5), np.uint16)
        for compression in (0, 1, 2, 3):
            blob = document(data, 3, 16, 2, compression)
            self.convert(blob[:-3], expect_error=True)
        self.convert(blob[:14] + bytes.fromhex("7fffffff") + blob[18:], expect_error=True)


if __name__ == "__main__":
    unittest.main()
