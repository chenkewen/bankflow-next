# BankFlow Next

BankFlow 第二代本地文件工作台。基于 TypeScript + Tauri + Rust，保留 Python PDF／OCR 解析引擎。当前版本 **0.1.5**。

## 功能

- 银行流水：电子 PDF、扫描件与图片转 Excel。
- PDF 工具：合并、拆分、提取页面、旋转及转图片。
- 图片工具：PNG、JPG、WEBP、BMP、TIFF 转换、缩放及合并为 PDF。
- 快捷圆盘：**Ctrl + 拖拽**打开银行流水圆盘，**Ctrl + Alt + 拖拽**打开通用圆盘。
- 个性设置：圆盘项目与排序、转换参数、主题配色、系统托盘。

文件在本机处理，不上传。结果默认保存在源文件旁，保留原文件；OCR 结果请核对金额与余额。

## 界面预览

### 文件工作台
![文件工作台](docs/images/workspace.png)

### 通用圆盘设置
![通用圆盘设置](docs/images/ring-settings.png)

### 银行流水快捷圆盘
![银行流水快捷圆盘](docs/images/bank-ring.png)

## 本地开发

需要 Node.js、Python、Rust 与 Windows MSVC／SDK。

```powershell
python -m venv .venv
.venv/Scripts/python.exe -m pip install -r requirements.txt
npm install
npm run desktop
```

打包：`npm run desktop:build`（需安装 PyInstaller）。安装包不内置 WebView2；电脑已有则复用，缺少时需联网安装。

## 目录

```text
frontend/          TypeScript 界面
src-tauri/         Rust 桌面集成与任务管理
python/bankflow/   Python 控制层、服务层与 PDF/OCR 解析器
resources/brand/   品牌图标
scripts/           启动与构建脚本
verification/      验证脚本
docs/images/       界面截图
```
