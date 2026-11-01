"""源码与冻结引擎共用的启动入口。"""
import sys
from pathlib import Path

if not getattr(sys, "frozen", False):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))

from bankflow.controllers.worker import main

if __name__ == "__main__":
    raise SystemExit(main())
