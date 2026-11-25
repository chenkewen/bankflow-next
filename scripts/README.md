# Windows x64 开发环境

安装 Node.js、Rust MSVC 工具链、Visual Studio C++ 生成工具、Windows SDK 和 Python 3.12。在仓库根目录运行以下命令，准备 PDFium、ONNX Runtime、OCR 模型及测试依赖。

```powershell
py -3.12 -m venv .venv
.\.venv\Scripts\pip.exe install -r requirements-verify.txt
```

构建脚本从上述开发环境提取原生 DLL 和模型并校验哈希。安装版与免安装版使用随包提供的运行资源。构建和测试命令见[项目 README](../README.md#从源码开发)。
