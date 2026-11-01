"""桌面端转换桥接：标准输出仅传输 JSON 行，银行解析器保持原样。"""

import contextlib
import json
import os
import sys
import threading

from bankflow.services.conversion import convert

PROTOCOL = sys.stdout


def emit(kind, **fields):
    PROTOCOL.write(json.dumps({"kind": kind, **fields}, ensure_ascii=False) + "\n")
    PROTOCOL.flush()


def listen_cancel(cancelled):
    # 父进程关闭管道时也取消，避免关闭桌面程序后继续导出。
    while line := read_line():
        try:
            if json.loads(line).get("cancel") is True:
                cancelled.set()
        except (ValueError, AttributeError):
            cancelled.set()
    cancelled.set()


def read_line():
    # 不持有 Python 缓冲输入锁，避免取消监听线程阻塞解释器正常退出。
    data = bytearray()
    while chunk := os.read(sys.stdin.fileno(), 1):
        if chunk == b"\n":
            break
        data.extend(chunk)
    return data.decode("utf-8")



def main():
    sys.stdout.reconfigure(encoding="utf-8", newline="\n")
    sys.stderr.reconfigure(encoding="utf-8", newline="\n")
    request = {}
    try:
        request = json.loads(read_line())
        if not isinstance(request, dict):
            raise ValueError("请求必须为 JSON 对象")
        # Windows 原生依赖在初始化时可能访问标准句柄，先加载再启动阻塞读线程。
        with contextlib.redirect_stdout(sys.stderr):
            from bankflow.parsers import pdf_to_excel
            if request.get("mode") == "ocr":
                from bankflow.parsers import ocr_to_excel
        cancelled = threading.Event()
        threading.Thread(target=listen_cancel, args=(cancelled,), daemon=True).start()
        convert(request, cancelled, emit)
        return 0
    except Exception as error:
        message = str(error)
        password = request.get("password") if isinstance(request, dict) else None
        if password:
            message = message.replace(password, "[已隐藏]")
        emit("error", message=f"转换失败：{message}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
