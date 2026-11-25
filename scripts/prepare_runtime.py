"""校验并准备 Windows x64 原生运行资源。"""
import hashlib
import shutil
import tempfile
from importlib.util import find_spec
from pathlib import Path

root = Path(__file__).resolve().parents[1]
engine = root / "build/native-runtime"

resources = {
    "pypdfium2_raw/pdfium.dll": "69f1e860f6742fbaafcc4db80e785eec9a4ee0da36ea6d1dc0867ea0f18baea6",
    "onnxruntime/capi/onnxruntime.dll": "e45f61e50d8650885452be538a00bb14a715e46a1bc3691000ce947e89ed0306",
    "rapidocr_onnxruntime/models/ch_PP-OCRv4_det_infer.onnx": "d2a7720d45a54257208b1e13e36a8479894cb74155a5efe29462512d42f49da9",
    "rapidocr_onnxruntime/models/ch_PP-OCRv4_rec_infer.onnx": "48fc40f24f6d2a207a2b1091d3437eb3cc3eb6b676dc3ef9c37384005483683b",
}


def verified_resources():
    files = {}
    for relative, expected in resources.items():
        package, _, filename = relative.partition("/")
        spec = find_spec(package)
        if spec is None or not spec.submodule_search_locations:
            raise RuntimeError(f"缺少构建依赖：{package}，请安装 requirements.txt")
        source = Path(next(iter(spec.submodule_search_locations))) / filename
        with source.open("rb") as handle:
            actual = hashlib.file_digest(handle, "sha256").hexdigest()
        if actual != expected:
            raise RuntimeError(f"资源校验失败：{relative}，请核对 Windows x64 构建依赖版本")
        files[relative] = source
    return files


if __name__ == "__main__":
    from prepare_avif import prepare

    files = {Path(relative).name: source for relative, source in verified_resources().items()}
    files["avifdec.exe"] = prepare()
    for name, source in files.items():
        destination = engine / "models" / name if name.endswith(".onnx") else engine / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=destination.parent) as stage:
            target = Path(stage) / name
            shutil.copyfile(source, target)
            target.replace(destination)
    print(engine)
