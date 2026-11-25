"""将原有 JSON 行调用转发给 Rust 引擎。"""
import json
import subprocess
import sys
from convert import command


def main():
    sys.stdout.reconfigure(encoding="utf-8", newline="\n")
    sys.stderr.reconfigure(encoding="utf-8", newline="\n")
    if len(sys.argv) != 1:
        print("用法：python scripts/run_worker.py，从标准输入读取任务", file=sys.stderr)
        return 2
    try:
        # 沿用调用方的输入管道，使取消信号和断开事件直接送达 Rust。
        with subprocess.Popen([*command(), "--image-worker"], stdout=subprocess.PIPE) as process:
            succeeded = False
            for line in process.stdout:
                sys.stdout.buffer.write(line)
                sys.stdout.buffer.flush()
                succeeded = json.loads(line).get("kind") in {"completed", "cancelled"}
            status = process.wait()
        return status or int(not succeeded)
    except Exception as error:
        print(json.dumps({"kind": "error", "message": str(error)}, ensure_ascii=False))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
