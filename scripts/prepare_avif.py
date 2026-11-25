"""准备与 Pillow 解码行为一致的官方 Windows x64 AVIF 解码器。"""
import hashlib
import shutil
import tempfile
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
URL = "https://github.com/AOMediaCodec/libavif/releases/download/v1.4.2/windows-artifacts.zip"
ARCHIVE_SHA256 = "cb2d9fea43dcbab1d0707e3b37eb7b08070ad2fb60a2c188c39ec12382c0484a"
EXE_SHA256 = "a03664ddffb9d847eb484af6ceeffce96e03d641da5c577d26662487dee689f2"


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def prepare():
    directory = ROOT / "build" / "codecs"
    directory.mkdir(parents=True, exist_ok=True)
    output = directory / "avifdec.exe"
    if output.is_file() and digest(output) == EXE_SHA256:
        return output
    with tempfile.TemporaryDirectory(dir=directory) as temp:
        archive = Path(temp) / "windows-artifacts.zip"
        with urllib.request.urlopen(URL, timeout=60) as download, archive.open("wb") as target:
            shutil.copyfileobj(download, target)
        if digest(archive) != ARCHIVE_SHA256:
            raise ValueError("AVIF 官方发布包校验失败")
        with zipfile.ZipFile(archive) as package:
            package.extract("avifdec.exe", temp)
        exe = Path(temp) / "avifdec.exe"
        if digest(exe) != EXE_SHA256:
            raise ValueError("AVIF 解码器校验失败")
        exe.replace(output)
    return output


if __name__ == "__main__":
    print(prepare())
