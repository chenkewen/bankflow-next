"""银行流水命令行启动脚本。"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from bankflow.controllers.cli import main

if __name__ == "__main__":
    raise SystemExit(main())
