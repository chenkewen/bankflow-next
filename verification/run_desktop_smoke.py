"""启动独立的 Windows Tauri/WebView2 实例，运行真实桌面命令回归。"""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from urllib.error import URLError
from urllib.request import urlopen
from uuid import uuid4

from PIL import Image
from smoke_worker import create_statement


ROOT = Path(__file__).resolve().parents[1]


def port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_for(url, process, accept):
    deadline = time.monotonic() + 50
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"诊断进程提前退出 ({process.returncode})")
        try:
            with urlopen(url, timeout=1) as response:
                if accept(response.read()):
                    return
        except (URLError, TimeoutError, ValueError):
            pass
        time.sleep(0.2)
    raise TimeoutError(f"诊断端点未就绪 {url}")


def stop(process):
    if process.poll() is None:
        # 只结束本轮创建且仍存活的进程树，包括 Vite/esbuild 和 WebView2。
        subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        process.wait(timeout=15)


def clean_workspace(temporary):
    # 应用退出后，WebView2 仍可能短暂持有测试配置目录中的日志。
    deadline = time.monotonic() + 15
    while True:
        try:
            temporary.cleanup()
            return
        except OSError as error:
            if error.winerror not in (32, 33, 145) or time.monotonic() >= deadline:
                raise
            time.sleep(0.2)


def run():
    if sys.platform != "win32":
        raise SystemExit("真实桌面验证需要 Windows 和 WebView2。")
    node = shutil.which("node")
    if not node:
        raise RuntimeError("找不到 Node.js")
    app_id = "local.bankflow.smoke.t" + uuid4().hex
    vite_port = port()
    dev_url = f"http://127.0.0.1:{vite_port}"
    env = os.environ.copy()
    env["TAURI_CONFIG"] = json.dumps({
        "identifier": app_id, "build": {"devUrl": dev_url}, "bundle": {"resources": []},
    })
    cargo = ["cargo", "build", "--locked", "--manifest-path", "src-tauri/Cargo.toml",
             "--bin", "bankflow-workbench"]
    target = Path(env.get("CARGO_TARGET_DIR", ROOT / "src-tauri/target"))
    with ExitStack() as stack:
        temporary = tempfile.TemporaryDirectory(prefix="bankflow-desktop-smoke-")
        stack.callback(clean_workspace, temporary)
        workspace = Path(temporary.name)
        # 调试构建能共享 Cargo 依赖缓存。恢复常规配置后再运行隔离的副本。
        try:
            subprocess.run(cargo, cwd=ROOT, env=env, check=True)
            app = Path(shutil.copy2(target / "debug/bankflow-workbench.exe", workspace))
        finally:
            subprocess.run(cargo, cwd=ROOT, check=True)
        config = Path(os.environ["APPDATA"]) / app_id
        data = Path(os.environ["LOCALAPPDATA"]) / app_id
        for folder in {config, data}:
            if folder.exists():
                raise RuntimeError("测试专用的应用目录已被占用")
            stack.callback(lambda p=folder: shutil.rmtree(p) if p.exists() else None)
        create_statement(workspace / "a.pdf", pages=2)
        create_statement(workspace / "b.pdf")
        create_statement(workspace / "cancel-source.pdf", pages=40)
        (workspace / "broken.pdf").write_bytes(b"%PDF-1.7\ninvalid\n")
        Image.new("RGB", (72, 40), "teal").save(workspace / "gesture-sample.png")

        vite = subprocess.Popen([node, ROOT / "node_modules/vite/bin/vite.js", "--host",
                                 "127.0.0.1", "--port", str(vite_port), "--strictPort"],
                                cwd=ROOT, creationflags=subprocess.CREATE_NO_WINDOW)
        stack.callback(stop, vite)
        wait_for(dev_url, vite, lambda data: b"/src/main.ts" in data)
        debug_port = port()
        cdp = f"http://127.0.0.1:{debug_port}"
        env.update(WEBVIEW2_USER_DATA_FOLDER=str(workspace / "webview"),
                   WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=f"--remote-debugging-port={debug_port}",
                   BANKFLOW_SMOKE_DIR=str(workspace), BANKFLOW_SMOKE_CDP=cdp,
                   BANKFLOW_SMOKE_APP_ID=app_id)
        process = subprocess.Popen([app], cwd=workspace, env=env,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
        stack.callback(stop, process)

        def windows_ready(raw):
            urls = [t["url"] for t in json.loads(raw) if t["type"] == "page"]
            return (any(u.startswith(dev_url + "/") and "quick" not in u for u in urls)
                    and any(u.startswith(dev_url + "/") and "quick=1" in u for u in urls))

        wait_for(cdp + "/json/list", process, windows_ready)
        for test in ["native_tasks_smoke.cjs", "desktop_preferences_smoke.cjs"]:
            subprocess.run([node, ROOT / "verification" / test], cwd=ROOT, env=env,
                           check=True, timeout=150)
        if process.wait(timeout=15) != 0:
            raise RuntimeError(f"桌面测试应用退出异常 ({process.returncode})")
        if not (config / "desktop.json").is_file():
            raise AssertionError("未在测试应用的独立目录写入桌面偏好")


def run_release(executable):
    # 使用实际发布产物，在空目录启动，用独立的 WebView2 配置核对双击时的首页。
    app = executable.resolve(strict=True)
    with ExitStack() as stack:
        temporary = tempfile.TemporaryDirectory(prefix="bankflow-release-smoke-")
        stack.callback(clean_workspace, temporary)
        debug_port = port()
        cdp = f"http://127.0.0.1:{debug_port}"
        source = Path(temporary.name) / "statement.pdf"
        create_statement(source)
        env = dict(os.environ, WEBVIEW2_USER_DATA_FOLDER=str(Path(temporary.name) / "webview"),
                   WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=f"--remote-debugging-port={debug_port}",
                   BANKFLOW_SMOKE_CDP=cdp, BANKFLOW_SMOKE_STATEMENT=str(source))
        process = subprocess.Popen([app], cwd=temporary.name, env=env,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
        stack.callback(stop, process)
        wait_for(cdp + "/json/list", process,
                 lambda raw: any(t["type"] == "page" for t in json.loads(raw)))
        subprocess.run(["node", ROOT / "verification/release_startup_smoke.cjs"],
                       cwd=ROOT, env=env, check=True, timeout=40)
        if process.wait(timeout=15) != 0:
            raise RuntimeError(f"发布版退出异常 ({process.returncode})")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", type=Path, nargs="?",
                        const=ROOT / "src-tauri/target/release/bankflow-workbench.exe",
                        help="直接验收已有的发布版 EXE，可指定路径")
    options = parser.parse_args()
    if options.release is None:
        run()
    else:
        run_release(options.release)
