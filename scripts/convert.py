"""兼容原有命令行，PDF 与 OCR 都由 Rust 引擎完成。"""
import os
import shutil
import subprocess
import sys
from pathlib import Path


def command():
    custom = os.environ.get("BANKFLOW_CONVERT_ENGINE")
    if custom:
        return [custom]
    cargo = shutil.which("cargo")
    if cargo:
        # Cargo 会检查整个项目及构建配置，单个源码文件的时间戳无法判断产物是否最新。
        manifest = Path(__file__).resolve().parents[1] / "src-tauri/Cargo.toml"
        return [cargo, "run", "--locked", "--quiet", "--manifest-path", str(manifest), "--"]
    raise FileNotFoundError("找不到 Rust 转换程序，请用 BANKFLOW_CONVERT_ENGINE 指定可执行文件。")


if __name__ == "__main__":
    try:
        code = subprocess.call([*command(), "--convert", *sys.argv[1:]])
    except OSError as error:
        print(f"无法启动转换程序：{error}", file=sys.stderr)
        code = 1
    raise SystemExit(code)
