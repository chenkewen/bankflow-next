"""构建含 Python 运行时和 OCR 模型的独立引擎。"""
import subprocess
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[1]
subprocess.run([
    sys.executable, "-m", "PyInstaller", "--noconfirm", "--onedir", "--console",
    "--name", "bankflow-worker", "--icon", str(root / "resources/brand/app.ico"), "--distpath", "build/engine",
    "--workpath", "build/pyinstaller", "--specpath", "build", "--paths", "python",
    "--collect-data", "rapidocr_onnxruntime",
    "--exclude-module", "cv2", "--exclude-module", "shapely",
    "--exclude-module", "scipy", "--exclude-module", "matplotlib",
    "--exclude-module", "pandas", "--exclude-module", "onnxruntime.tools",
    "--exclude-module", "onnxruntime.quantization", "--exclude-module", "onnxruntime.transformers",
    "--exclude-module", "PySide6", "--exclude-module", "tkinter",
    "scripts/run_worker.py",
], cwd=root, check=True)
