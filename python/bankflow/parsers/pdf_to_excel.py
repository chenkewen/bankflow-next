"""
PDF 银行流水 -> Excel 智能转换引擎 (v4.1 - 通用自适应版)
核心策略：
1. 通用关键词集（替代特定银行模板）+ 经典词聚类分列算法
2. 水印/印章过滤
3. 多线程并行处理
4. 智能行合并（处理摘要换行）
"""

import re
import sys
import json
import logging
import argparse
import math
import os
from pathlib import Path
from typing import Optional, List, Tuple, Dict, Any, Callable
from concurrent.futures import ThreadPoolExecutor, as_completed

import pdfplumber
from pdfminer.pdfdocument import PDFEncryptionError
import openpyxl
from openpyxl.styles import Font, PatternFill, Alignment, Border, Side
from openpyxl.utils import get_column_letter

# ─── 日志配置 ───────────────────────────────────────────────
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger('pdf_converter')

# ─── 通用关键词集（覆盖主流银行流水表头用词） ────────────────
UNIVERSAL_HEADER_KW = {
    # 日期时间类
    '日期', '交易日期', '记账日期', '交易日', '记账时间', '交易时间', '时间',
    # 金额类
    '金额', '交易金额', '借方', '贷方', '借方金额', '贷方金额', '收入', '支出',
    '借方(出账)', '贷方(入账)',
    # 余额类
    '余额', '本次余额', '账户余额', '本次金额',
    # 摘要/备注类
    '摘要', '交易摘要', '用途', '备注', '附言', '交易附言',
    # 对方信息
    '对方户名', '对方账户', '对方账号', '对方卡号', '对方开户行', '对手信息', '对于信息',
    '收(付)方', '收(付)方名称', '收(付)方账号',
    # 序号/流水
    '序号', '流水号', '企业流水号', '日志号', '交易流水号', '水号',
    # 渠道
    '渠道', '交易渠道', '交易名称', '交易介质',
    # 其他
    '凭证种', '凭证号', '网点名称', '交易类型', '交易流',
}

# 跳过行的通用正则
UNIVERSAL_SKIP_PATTERNS = [
    re.compile(r'第\s*\d+\s*页'),
    re.compile(r'温馨提示'),
    re.compile(r'本次查询时间'),
    re.compile(r'本页合计'),
    re.compile(r'-{4,}END-{4,}'),
    re.compile(r'该交易明细因不可预测'),
    re.compile(r'以上内容仅供参考'),
    re.compile(r'打印日期'),
    re.compile(r'Page\s*\d+'),
    # 浏览器截图PDF中的URL和页眉页脚
    re.compile(r'https?://'),
    re.compile(r'…?\s*\d+\s*/\s*\d+\s*$'),  # "2/32" 或 "…2/32" 页码
    re.compile(r'^\d{4}/\d{1,2}/\d{1,2}\s+\d{1,2}:\d{2}\s*$'),  # 纯日期时间页眉
    # 招商银行等PDF页脚内容
    re.compile(r'交易流水验真'),
    re.compile(r'www\.\w+'),
    re.compile(r'提交系统验真'),
    re.compile(r'点击.*查询'),
]

# ─── 通用日期模式（记录分组/跨页合并/日期列检测共用） ────────
# 覆盖主流银行的日期写法：
#   2024-01-02（招行等）、2024/01/02、2024.01.02、
#   20240102（工行/建行等常用8位无分隔）、2024年1月2日
DATE_START_PATTERNS = [
    re.compile(r'^\d{4}[-‑–]\d{1,2}[-‑–]\d{1,2}'),
    re.compile(r'^\d{4}/\d{1,2}/\d{1,2}'),
    re.compile(r'^\d{4}\.\d{1,2}\.\d{1,2}'),
    # 8位无分隔日期必须校验月/日取值范围，避免把普通8位数字误判为日期
    re.compile(r'^\d{4}(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])(?!\d)'),
    re.compile(r'^\d{4}年\d{1,2}月\d{1,2}日'),
]

# 内容级日期检测（不要求出现在行首），用于自动识别日期列。
# (?<!\d) 前置边界防止在卡号/流水号等长数字串内部误匹配。
DATE_CONTENT_RE = re.compile(
    r'(?<!\d)\d{4}(?:[-‑–/.]\d{1,2}[-‑–/.]\d{1,2}'
    r'|(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])(?!\d)'
    r'|年\d{1,2}月\d{1,2}日)')


def starts_with_date(text: str) -> bool:
    """判断文本是否以日期开头（支持多种主流银行日期格式）。"""
    t = text.strip()
    return any(p.match(t) for p in DATE_START_PATTERNS)


def extract_year(val: str) -> Optional[str]:
    """从日期文本开头提取4位年份，无法识别时返回 None。"""
    m = re.match(
        r'(\d{4})(?:[-‑–/.年]'
        r'|(?=(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])(?!\d)))', val)
    return m.group(1) if m else None


# Excel 样式常量
META_SEP = re.compile(r'(?:(?<!\d)[：:]|[：:](?!\d))(?!//)')
ILLEGAL_RE = re.compile(r'[\x00-\x08\x0b\x0c\x0e-\x1f]')
_DATE_SPACE_RE = re.compile(r'(\d{4})\s*[-‑–]\s*(\d{1,2})\s*[-‑–]\s*(\d{1,2})')

# ─── 时间冒号修复（银行 PDF 字体子集 ToUnicode 映射损坏） ──────
# 部分银行 PDF 的字体做了子集化，同一个冒号存在多个 glyph，其中一个的
# Unicode 映射缺失，导致时间中的冒号被读成异常字符：
#   U+0000(NULL)   兴业银行部分样本。它会被 ILLEGAL_RE 当控制字符删除，
#                  使 "17:27:32" 变成 "1727:32"（时间整体错位）
#   U+2236(∶ 比例符号)  兴业银行另一批样本。渲染时两侧留白，
#                  肉眼看是 "17 : 27 : 32"，且非标准冒号不便后续处理
# NULL 的还原必须限定在时间专属模式内：同一份 PDF 中 NULL 还承载了撇号、
# 标点等其他损坏字符（如 "Counterpa\x00y\x00s" 实为 "Counterparty's"），
# 一概替换会写坏摘要与账号。因此按损坏程度分两种精准模式处理：
#   1. 两个冒号都坏（HH\x00MM\x00SS）——以日期前缀锚定，避免误伤金额千分位
#   2. 仅首个冒号坏（HH\x00MM:SS）——以后随的正常冒号锚定
_NULL_TIME_FULL_RE = re.compile(
    r'(?<=\d{2}-\d{2} )(\d{1,2})\x00(\d{2})\x00(\d{2})')
_NULL_COLON_RE = re.compile(r'(?<=\d)\x00(?=\d{2}:)')
_RATIO_COLON = '∶'
# grid 模式下 pdfplumber 用 \n 表示单元格内换行。"日期\n时间" 若直接删除换行
# 会粘连成 "2024-12-0510:12:00"，故先在两者之间补空格。
# 限定换行后必须是完整的 HH:MM:SS，以免误伤时间内部换行的情况
# （如兴业 "15\x005\n0\x0048"，那属于上面的冒号损坏场景）。
_DATE_TIME_NEWLINE_RE = re.compile(
    r'(?<=\d)[\r\n]+(?=\d{2}[:：∶\x00]\d{2}[:：∶\x00]\d{2})')
# 时间中的全角冒号归一化为半角（限定数字上下文，不影响摘要等正文）
_FULLWIDTH_COLON_RE = re.compile(r'(?<=\d)：(?=\d{2})')
# 清除时间冒号两侧的多余空格
_TIME_COLON_SPACE_RE = re.compile(r'(?<=\d)\s+:\s*(?=\d{2})|(?<=\d):\s+(?=\d{2})')

C_HDR_BG = '2E75B6'; C_HDR_FG = 'FFFFFF'
C_MK_BG = 'DCE6F1'; C_ALT = 'F2F7FF'
C_BDR = 'B8CCE4'
THREAD_THRESHOLD = 20


# ═══════════════════════════════════════════════════════
# 通用工具函数（来自 v3.1 已验证逻辑）
# ═══════════════════════════════════════════════════════

def sanitize(v: object) -> str:
    """清洗单元格文本。"""
    if v is None: return ''
    s = str(v)
    # 日期与时间之间的换行须先补空格，否则下一步删除换行会使两者粘连
    s = _DATE_TIME_NEWLINE_RE.sub(' ', s)
    s = s.replace('\n', '').replace('\r', '')
    s = re.sub(r' {2,}', ' ', s).strip()
    if re.match(r'^[-—=\s]{3,}$', s): return ''
    # 冒号修复必须在删除控制字符之前：NULL 一旦被删就无法还原
    s = s.replace(_RATIO_COLON, ':')
    s = _NULL_TIME_FULL_RE.sub(r'\1:\2:\3', s)
    s = _NULL_COLON_RE.sub(':', s)
    s = _FULLWIDTH_COLON_RE.sub(':', s)
    s = _TIME_COLON_SPACE_RE.sub(':', s)
    s = ILLEGAL_RE.sub('', s)
    s = s.encode('utf-16', 'surrogatepass').decode('utf-16', 'ignore')
    s = _DATE_SPACE_RE.sub(
        lambda m: f'{m.group(1)}-{m.group(2).zfill(2)}-{m.group(3).zfill(2)}', s)
    return s.strip()


def is_skip_row(row: list) -> bool:
    """判断是否应跳过此行。"""
    text = ' '.join(str(c) for c in row if c)
    if not text.strip():
        return True
    for pat in UNIVERSAL_SKIP_PATTERNS:
        if pat.search(text):
            return True
    return False


def drop_skip_lines(chars: list) -> list:
    """剔除字符集合中命中跳过模式的整行（页眉、页脚、页码等）。

    zones 模式下，若某页没有分隔线（常见于跨物理页的续页），整页会形成
    一个 zone，页脚会和数据落在一起。此时 is_skip_row 会因"第 N 页"之类
    的页脚文字把整个 zone 的数据一并丢弃，导致该页记录整条消失。
    先按行剔除页脚，再整体分列即可保住数据。

    Args:
        chars: 字符字典列表

    Returns:
        剔除页眉页脚行后的字符列表
    """
    if not chars:
        return chars
    keep = []
    for line in group_by_y(chars, tol=4.0):
        text = ''.join(c['text'] for c in line)
        if any(p.search(text) for p in UNIVERSAL_SKIP_PATTERNS):
            continue
        keep.extend(line)
    return keep


def filter_wm(page: pdfplumber.page.Page) -> pdfplumber.page.Page:
    """过滤水印和印章字符。"""
    def keep_char(obj: dict) -> bool:
        if obj.get('object_type') != 'char':
            return True
        # 过滤旋转文字（斜向水印）：matrix 的 b、c 分量非零表示字符被旋转。
        # 兴业银行等流水会铺满斜向重复水印（如"兴业银行 验证码 打印时间"），
        # 这些水印字与正文同色同字号，仅靠颜色/字号无法区分，必须靠旋转角度过滤。
        m = obj.get('matrix')
        if m and (abs(m[1]) > 0.01 or abs(m[2]) > 0.01):
            return False
        color = obj.get('non_stroking_color')
        if color is not None:
            # 浅色（水印）过滤
            if isinstance(color, (int, float)) and color > 0.72:
                return False
            if isinstance(color, (list, tuple)) and len(color) >= 3:
                if all(isinstance(c, (int, float)) and c > 0.72 for c in color[:3]):
                    return False
                # 红色印章过滤
                r, g, b = color[0], color[1], color[2]
                if isinstance(r, (int, float)) and r > 0.7 and g < 0.3 and b < 0.3:
                    return False
        size = obj.get('size', 12)
        if size < 4 or size > 80:
            return False
        return True
    try:
        return page.filter(keep_char)
    except Exception:
        return page


def group_by_y(items: list, tol: float = 4.0) -> list:
    """将文本元素按 Y 坐标分组为逻辑行。"""
    if not items:
        return []
    it = sorted(items, key=lambda x: x['top'])
    rows, cur, cy = [], [it[0]], it[0]['top']
    for x in it[1:]:
        if x['top'] - cy <= tol:
            cur.append(x)
        else:
            rows.append(sorted(cur, key=lambda i: i['x0']))
            cur, cy = [x], x['top']
    rows.append(sorted(cur, key=lambda i: i['x0']))
    return rows


def _clean_col_name(name: str) -> str:
    """清洗列名，移除中文词之间多余的空格。"""
    res, chars = [], list(name)
    for i, c in enumerate(chars):
        if c == ' ':
            # 两个中文字之间的空格去掉
            if (i > 0 and i < len(chars) - 1
                    and ord(chars[i - 1]) > 0x2E7F
                    and ord(chars[i + 1]) > 0x2E7F):
                continue
        res.append(c)
    return ''.join(res)


def build_cols(header_groups: list, page_width: float) -> tuple:
    """
    核心：将表头行中的词按 X 坐标聚类，自动识别列名和列边界。
    支持多行换行表头：同一列中不同行的文字会按 top 排序后纵向拼接。

    Args:
        header_groups: group_by_y 返回的表头区域行列表
        page_width: 页面宽度

    Returns:
        (列名列表, 列边界列表)
    """
    # 将所有表头词扁平化并按 x0 排序
    all_w = sorted(
        [w for g in header_groups for w in g],
        key=lambda w: w['x0']
    )
    if not all_w:
        return [], []

    # 按 x0 坐标聚类（间距 < 12px 视为同一列）
    clusters, cur = [], [all_w[0]]
    for w in all_w[1:]:
        if abs(w['x0'] - cur[-1]['x0']) < 12:
            cur.append(w)
        else:
            clusters.append(cur)
            cur = [w]
    clusters.append(cur)

    # 从每个聚类提取列名和边界
    # 关键改进：按 top 分层，将不同行的文字纵向拼接（而非简单横向拼接）
    names, bounds = [], []
    for i, cl in enumerate(clusters):
        # 按 top 值分层（同一行的词 top 值相近）
        cl_sorted = sorted(cl, key=lambda w: w['top'])
        layers = []
        layer_cur = [cl_sorted[0]]
        for w in cl_sorted[1:]:
            if abs(w['top'] - layer_cur[-1]['top']) < 5:
                layer_cur.append(w)
            else:
                layers.append(layer_cur)
                layer_cur = [w]
        layers.append(layer_cur)

        # 每层内按 x0 排序拼接文字，层间直接拼接（不加空格，因为是同一个词的换行）
        layer_texts = []
        for layer in layers:
            layer.sort(key=lambda w: w['x0'])
            text = ''.join(w['text'] for w in layer)
            if text not in layer_texts:  # 去重
                layer_texts.append(text)
        # sanitize 兜底清除控制字符，避免列名写入 Excel 时报非法字符
        name = sanitize(_clean_col_name(''.join(layer_texts)))

        x0 = min(w['x0'] for w in cl)
        x1 = (clusters[i + 1][0]['x0'] - 0.5
               if i + 1 < len(clusters) else page_width)
        names.append(name)
        bounds.append((x0 - 4, x1))
    return names, bounds


def chars_to_row(chars: list, col_bounds: list) -> list:
    """将一组字符按列边界分桶，组成一行数据。

    支持多行合并场景：同一列内不同 top 行的文字之间自动插入空格，
    避免文字粘连（如 '2024-01-02' 和 '7:04:48' 合并为 '2024-01-02 7:04:48'）。

    Args:
        chars: 字符字典列表，每个包含 text/x0/x1/top/bottom
        col_bounds: 列边界元组列表 [(x_start, x_end), ...]

    Returns:
        每列文本组成的字符串列表
    """
    buckets = [[] for _ in col_bounds]
    for ch in chars:
        cx = (ch['x0'] + ch['x1']) / 2
        assigned = -1
        for i, (xs, xe) in enumerate(col_bounds):
            if xs <= cx <= xe:
                assigned = i
                break
        # 如果没命中任何列，找最近的列
        if assigned == -1:
            best_i, best_d = -1, float('inf')
            for i, (xs, xe) in enumerate(col_bounds):
                d = min(abs(cx - xs), abs(cx - xe))
                if d < best_d:
                    best_d, best_i = d, i
            if best_d <= 15:
                assigned = best_i
        if assigned >= 0:
            buckets[assigned].append(ch)
    row = []
    for b in buckets:
        b.sort(key=lambda c: (round(c['top'], 1), c['x0']))
        # 在不同 top 行的文字之间插入空格，避免粘连
        parts = []
        prev_top = None
        for c in b:
            cur_top = round(c['top'], 1)
            if prev_top is not None and abs(cur_top - prev_top) > 3:
                parts.append(' ')
            parts.append(c['text'])
            prev_top = cur_top
        row.append(sanitize(''.join(parts)))
    return row


def _is_encryption_error(e: BaseException) -> bool:
    """判断异常是否为 PDF 加密/密码错误。

    pdfplumber 会把 pdfminer 的异常包装为 PdfminerException(原异常)，
    原始异常存放在 args 中（而非 __cause__），因此需要同时检查两条链。
    """
    if isinstance(e, PDFEncryptionError):
        return True
    for arg in getattr(e, 'args', ()):
        if isinstance(arg, PDFEncryptionError):
            return True
    cause = e.__cause__ or e.__context__
    if cause is not None and cause is not e:
        return _is_encryption_error(cause)
    return False


def probe_pdf_encryption(path: str, password: Optional[str] = None) -> str:
    """探测 PDF 是否需要打开密码。

    Args:
        path: PDF 文件路径
        password: 尝试使用的密码，None 表示不提供密码

    Returns:
        'ok'            — 可正常打开（未加密，或密码正确）
        'need_password' — 已加密且未提供密码/密码不正确
    """
    try:
        with pdfplumber.open(path, password=password or ''):
            pass
        return 'ok'
    except Exception as e:
        # 其他异常（文件损坏等）留给正式解析阶段报告
        return 'need_password' if _is_encryption_error(e) else 'ok'


# ═══════════════════════════════════════════════════════
# 核心解析引擎（v4.1）
# ═══════════════════════════════════════════════════════

class BankPDFParser:
    def __init__(self, path: str,
                 password: Optional[str] = None,
                 progress_callback: Optional[Callable[[int, int], None]] = None,
                 cancel_check: Optional[Callable[[], bool]] = None):
        self.path = path
        self.password = password
        self.metadata = []
        self.col_names = []
        self.col_bounds = []
        self.data_rows = []
        self.hdr_bottom = 0.0
        # 首页主数据行的起始 X，用于校正后续页的整页水平偏移
        self._base_row_x: Optional[float] = None
        self._progress_callback = progress_callback
        self._cancel_check = cancel_check

    def parse(self):
        with pdfplumber.open(self.path, password=self.password or '') as pdf:
            total_pages = len(pdf.pages)
            fmt = self._classify(pdf)
            logger.info(
                f"解析: {Path(self.path).name} | 格式: {fmt} | 页数: {total_pages}")

            # 第一页：串行处理，获取布局基准
            self._process_first_page(pdf, fmt)
            if self._progress_callback:
                self._progress_callback(1, total_pages)

            # 后续页
            if total_pages > 1:
                if self._cancel_check and self._cancel_check():
                    logger.info("用户取消解析")
                elif total_pages > THREAD_THRESHOLD:
                    self._process_parallel(fmt, total_pages)
                else:
                    self._process_serial(pdf, range(1, total_pages), fmt)

        self.data_rows = self._merge_cont(self.data_rows)
        self.data_rows = self._merge_cross_page(self.data_rows)
        self._postprocess_dates()
        self._postprocess_incomplete_records()
        logger.info(f"解析完成: {len(self.data_rows)} 行数据, {len(self.col_names)} 列")
        return self

    def _classify(self, pdf):
        """自动判断 PDF 格式：grid (有线框) / zones (有横线) / text (纯文本)。"""
        for page in pdf.pages[:3]:
            try:
                tables = page.extract_tables(
                    {'snap_tolerance': 4, 'edge_min_length': 15})
                if tables and any(
                        len(t) >= 2 and max(len(r) for r in t) >= 3
                        for t in tables):
                    return 'grid'
                hlines = [
                    l for l in page.lines
                    if l.get('height', 1) < 1.5
                    and l.get('width', 0) > page.width * 0.25
                ]
                if len(hlines) >= 4:
                    return 'zones'
            except Exception:
                pass
        return 'text'

    def _process_first_page(self, pdf, fmt):
        page = filter_wm(pdf.pages[0])
        if fmt == 'grid':
            rows = self._extract_grid(page)
            if rows:
                # 预获取表格顶端 Y 坐标用于提取元数据
                meta_top = 150
                try:
                    found = page.find_tables()
                    if found:
                        meta_top = found[0].bbox[1]
                except Exception:
                    pass
                for row in rows:
                    if is_skip_row(row):
                        continue
                    if not self.col_names:
                        self.col_names = [_clean_col_name(c) for c in row]
                        self.metadata = self._extract_meta(page, meta_top)
                        continue
                    self.data_rows.append(row)
        else:
            wds = page.extract_words(x_tolerance=3)
            all_rows = group_by_y(wds, tol=5)
            hs, he, extra_bottom = self._find_header(all_rows)
            if hs >= 0:
                hdr_groups = all_rows[hs: he + 1]
                hdr_top = min(w['top'] for g in hdr_groups for w in g)
                self.hdr_bottom = max(
                    w['bottom'] for g in hdr_groups for w in g)
                # 如果有英文翻译行，将其底部坐标也纳入 hdr_bottom，
                # 避免英文翻译行被当作数据处理
                if extra_bottom > self.hdr_bottom:
                    self.hdr_bottom = extra_bottom
                self.col_names, self.col_bounds = build_cols(
                    hdr_groups, page.width)
                # 元数据仅搜索表头上方区域
                self.metadata = self._extract_meta(page, hdr_top - 5)
                logger.info(f"检测到 {len(self.col_names)} 列: {self.col_names}")
            else:
                self.metadata = self._extract_meta(page, page.height)
                logger.warning("未能自动检测到表头")
            self.data_rows.extend(
                self._extract_no_grid(page, fmt, is_first_page=True))

    def _process_serial(self, pdf, p_range, fmt):
        total_pages = len(pdf.pages)
        for i in p_range:
            if self._cancel_check and self._cancel_check():
                logger.info("用户取消解析")
                break
            page = filter_wm(pdf.pages[i])
            for r in self._extract_by_fmt(page, fmt):
                if not self._is_header_row(r):
                    self.data_rows.append(r)
            if self._progress_callback:
                self._progress_callback(i + 1, total_pages)

    def _process_parallel(self, fmt, total):
        num = min(os.cpu_count() or 4, 8)
        chunk_size = math.ceil((total - 1) / num)
        chunks = [
            range(i, min(i + chunk_size, total))
            for i in range(1, total, chunk_size)
        ]
        logger.info(f"启用多线程: {num} 线程处理 {total - 1} 页")
        res_map = {}
        with ThreadPoolExecutor(max_workers=num) as ex:
            futures = {
                ex.submit(self._worker, chunk, fmt): chunk
                for chunk in chunks
            }
            for f in as_completed(futures):
                try:
                    res_map.update(f.result())
                except Exception as e:
                    failed_pages = list(futures[f])
                    logger.error(
                        f"并行处理页面 {failed_pages} 失败: {e}")
        for i in range(1, total):
            if i in res_map:
                for r in res_map[i]:
                    if not self._is_header_row(r):
                        self.data_rows.append(r)
            if self._progress_callback and i % 5 == 0:
                self._progress_callback(i + 1, total)

    def _worker(self, p_indices, fmt):
        res = {}
        with pdfplumber.open(self.path, password=self.password or '') as pdf:
            for i in p_indices:
                res[i] = self._extract_by_fmt(filter_wm(pdf.pages[i]), fmt)
        return res

    def _extract_by_fmt(self, page, fmt):
        return (self._extract_grid(page)
                if fmt == 'grid' else self._extract_no_grid(page, fmt))

    def _extract_grid(self, page):
        """提取有线框表格的数据。"""
        for v, h in [('lines', 'lines'), ('lines_strict', 'lines_strict')]:
            try:
                tables = page.extract_tables({
                    'vertical_strategy': v,
                    'horizontal_strategy': h,
                    'snap_tolerance': 4,
                    'edge_min_length': 15,
                })
                if tables:
                    out = []
                    for t in tables:
                        for r in t:
                            c = [sanitize(x) for x in r]
                            if any(c) and not is_skip_row(c):
                                out.append(c)
                    return out
            except Exception:
                pass
        return []

    def _extract_no_grid(self, page, fmt, is_first_page: bool = False):
        """提取无线框表格的数据（核心逻辑）。

        Args:
            page: PDF 页面对象
            fmt: 'zones' 或 'text'
            is_first_page: 是否为首页。首页的 hdr_bottom 由 _find_header 现算，
                可信；后续页若检测不到表头则不可沿用（见下方 cutoff 说明）。
        """
        page_chars = self._chars_of(page)
        if not self.col_bounds:
            return []

        # 动态检测本页的表头底部位置。
        # 后续页可能在不同Y坐标重复表头（如招商银行第1页表头在 y≈285，
        # 而后续页表头可能在 y≈73），不能固定使用第1页的 hdr_bottom。
        page_hdr_bottom = self._detect_page_header_bottom(page)
        if page_hdr_bottom > 0:
            cutoff = page_hdr_bottom
        elif is_first_page:
            cutoff = self.hdr_bottom
        else:
            # 后续页检测不到表头，说明是跨物理页的"续页"（部分银行一个逻辑页
            # 会跨两个物理页，续页只有正文和页脚）。此时数据从页顶就开始，
            # 若沿用首页 hdr_bottom（其中含首页元数据区的高度）会整页被裁掉。
            cutoff = 0.0

        all_chars = [c for c in page_chars if c['top'] > cutoff]

        # 本页专用列边界：部分 PDF（尤其多批次导出后合并的）各页存在整页
        # 水平偏移，若沿用首页边界会整行串列。用局部变量而非改写
        # self.col_bounds，以保证多线程并行处理时互不干扰。
        bounds = self._page_col_bounds(all_chars, is_first_page)

        if fmt == 'zones':
            # 分隔线与 zone 起点都必须用本页的 cutoff，不能用首页 hdr_bottom：
            # 否则后续页位于首页表头高度之上的分隔线会被丢弃，
            # 该区域的记录随之整条丢失。
            sep_ys = sorted(set(
                round(l['top'], 1)
                for l in page.lines
                if (l.get('height', 1) < 1.5
                    and l.get('width', 0) > page.width * 0.25
                    and l['top'] > cutoff)
            ))
            zone_bounds = [cutoff] + sep_ys + [page.height + 1]
            res = []
            for i in range(len(zone_bounds) - 1):
                y0, y1 = zone_bounds[i], zone_bounds[i + 1]
                zc = [c for c in all_chars if y0 < c['top'] <= y1]
                # 先按行剔除页眉页脚：无分隔线的续页会把整页并成一个 zone，
                # 页脚若留在其中会令 is_skip_row 连数据一起丢弃
                zc = drop_skip_lines(zc)
                if zc:
                    row = chars_to_row(zc, bounds)
                    if any(row) and not is_skip_row(row):
                        res.append(row)
            # 已知局限：个别银行 PDF 会漏画记录之间的分隔线，使两条记录落入
            # 同一 zone 被压成一行（约占 6%）。曾尝试用最大间隙法对超大 zone
            # 二次切分，但会把"日期跨两行"排版的记录切散（日期变成
            # "2025-" 与 "12-31" 两条），反而更糟，故维持现状待更精准的方案。
            return res
        else:
            return self._extract_text_with_record_grouping(
                all_chars, page.width, bounds)

    def _row_start_x(self, chars: list) -> Optional[float]:
        """取本页主数据行的起始 X 坐标，作为页面水平位置的基准。

        主数据行指字符数达到该页最长行一半以上的文字行——这类行必然从
        第一列起排，其起始 X 能稳定反映整页的水平位置。取中位数以抵抗
        个别异常行的干扰。

        Args:
            chars: 数据区域的字符列表

        Returns:
            基准 X 坐标；页面无足够长的数据行时返回 None（不做校正）
        """
        if not chars:
            return None
        by_top: Dict[int, list] = {}
        for c in chars:
            by_top.setdefault(round(c['top']), []).append(c)
        if not by_top:
            return None
        max_len = max(len(v) for v in by_top.values())
        # 行太短说明本页没有完整数据行（如仅有页脚的尾页），不足以估算
        if max_len < 10:
            return None
        starts = sorted(min(c['x0'] for c in v)
                        for v in by_top.values() if len(v) >= max_len * 0.5)
        return starts[len(starts) // 2] if starts else None

    def _page_col_bounds(self, all_chars: list,
                         is_first_page: bool) -> list:
        """计算本页专用的列边界，校正整页水平偏移。

        同一份 PDF 由多批次导出合并时，各页页边距可能不一致（实测某农商行
        样本部分页整体右移约 10pt）。列边界只在首页推算一次，偏移页会整行
        串列——例如 9 位流水号的末 2 位越界落进日期列，使日期变成
        "51202510" 这类错值。这里以首页主数据行的起始 X 为基准，按本页
        偏移量整体平移列边界。

        Args:
            all_chars: 本页数据区域的字符列表
            is_first_page: 首页时记录基准值，后续页据此校正

        Returns:
            本页适用的列边界列表
        """
        cur_x = self._row_start_x(all_chars)
        if is_first_page:
            self._base_row_x = cur_x
            return self.col_bounds
        if cur_x is None or self._base_row_x is None:
            return self.col_bounds
        shift = cur_x - self._base_row_x
        # 小于 2pt 属于字体渲染的正常抖动，不予校正
        if abs(shift) < 2.0:
            return self.col_bounds
        return [(x0 + shift, x1 + shift) for x0, x1 in self.col_bounds]

    def _extract_text_with_record_grouping(
            self, all_chars: list, page_width: float,
            bounds: Optional[list] = None) -> list:
        """通用记录分组提取：自适应检测行间距来合并多行记录。

        许多银行PDF的每条记录跨多行（如招商银行每条3行），同一记录内
        的行间距较小，不同记录之间的间距较大。本方法自动检测这种双模态
        间距模式，将同一记录的所有字符合并后再整体分列。

        如果未检测到明显的双模态间距（所有行距相近），则退回逐行处理。

        Args:
            all_chars: 数据区域的所有字符列表
            page_width: 页面宽度

        Returns:
            解析后的数据行列表
        """
        # bounds 为 None 时退回全局列边界（供旧调用方与测试使用）
        if bounds is None:
            bounds = self.col_bounds
        if not bounds or not all_chars:
            return []

        # 先按y坐标分行
        char_rows = group_by_y(all_chars, tol=6)
        if not char_rows:
            return []

        # 过滤掉页眉页脚/URL/纯英文行等垃圾行（在间距计算前过滤，避免干扰分组）
        filtered_rows = []
        for rg in char_rows:
            line_text = ''.join(c['text'] for c in sorted(rg, key=lambda c: c['x0']))
            # 跳过通用过滤模式
            if any(p.search(line_text) for p in UNIVERSAL_SKIP_PATTERNS):
                continue
            # 跳过纯英文行（中英双语表头的英文翻译）
            plain = line_text.replace(' ', '')
            if plain and re.match(r'^[A-Za-z]+$', plain):
                continue
            filtered_rows.append(rg)
        char_rows = filtered_rows
        if not char_rows:
            return []

        # 如果只有1行，直接处理
        if len(char_rows) == 1:
            row = chars_to_row(char_rows[0], bounds)
            if any(row) and not is_skip_row(row):
                return [row]
            return []

        # 计算相邻行的间距
        row_tops = [min(c['top'] for c in rg) for rg in char_rows]
        gaps = [row_tops[i] - row_tops[i - 1] for i in range(1, len(row_tops))]

        if not gaps:
            return []

        sorted_gaps = sorted(gaps)
        min_gap = sorted_gaps[0]
        max_gap = sorted_gaps[-1]

        # 检测双模态间距：使用「最大间隙法」在排序后的唯一间距值中
        # 找到最大跳跃点作为分隔阈值。
        # 相比旧版 median*1.3，此方法能处理间距值多种分布的情况
        # （如招商银行：12/17.7/20.6/26.25 四种间距）。
        threshold = self._find_gap_threshold(gaps)

        if threshold is None:
            # 间距均匀，不需要合并，退回逐行处理
            rows = []
            for rg in char_rows:
                row = chars_to_row(rg, bounds)
                if any(row) and not is_skip_row(row):
                    rows.append(row)
            return rows

        # 基于间距阈值分组，同时结合内容验证
        record_groups = [[char_rows[0]]]
        for i in range(1, len(char_rows)):
            is_new_record = gaps[i - 1] > threshold

            # 内容级别验证：即使间距判定为续行，
            # 如果该行第一列以日期开头，也应视为新记录
            if not is_new_record:
                line_text = ''.join(
                    c['text'] for c in sorted(
                        char_rows[i], key=lambda c: c['x0']))
                if starts_with_date(line_text):
                    is_new_record = True

            if is_new_record:
                record_groups.append([char_rows[i]])
            else:
                record_groups[-1].append(char_rows[i])

        # 每组合并所有字符后分列
        rows = []
        for group in record_groups:
            merged_chars = []
            for rg in group:
                merged_chars.extend(rg)
            row = chars_to_row(merged_chars, bounds)
            if any(row) and not is_skip_row(row):
                rows.append(row)
        return rows

    @staticmethod
    def _find_gap_threshold(gaps: list) -> Optional[float]:
        """使用最大间隙法（Maximum Gap）检测行间距的分隔阈值。

        将排序后的唯一间距值序列中最大的跳跃点作为续行/新记录的分界线。
        相比简单的 median * factor，此方法对多种间距分布（如4种不同值）更稳健。

        Args:
            gaps: 相邻行之间的间距列表

        Returns:
            阈值（float），间距 > 阈值视为新记录；None 表示间距均匀无需合并
        """
        if not gaps:
            return None

        sorted_gaps = sorted(gaps)
        min_gap = sorted_gaps[0]
        max_gap = sorted_gaps[-1]

        # 间距差异不够大，视为均匀
        if min_gap <= 0 or max_gap / min_gap <= 1.25:
            return None

        # 按唯一间距值分组（四舍五入到小数点后1位避免浮点噪声）
        unique_sorted = sorted(set(round(g, 1) for g in gaps))

        if len(unique_sorted) <= 1:
            return None

        # 找相邻唯一值之间的最大跳跃
        best_jump = 0
        best_split_idx = 1
        for i in range(1, len(unique_sorted)):
            jump = unique_sorted[i] - unique_sorted[i - 1]
            if jump > best_jump:
                best_jump = jump
                best_split_idx = i

        small_gap = unique_sorted[best_split_idx - 1]
        big_gap = unique_sorted[best_split_idx]

        # 只有大间距/小间距比率 > 1.3 才认为有双模态
        if big_gap / small_gap < 1.3:
            return None

        # 阈值取跳跃中点
        threshold = (small_gap + big_gap) / 2

        logger.debug(
            f"最大间隙法检测: 唯一间距={unique_sorted} "
            f"分裂点={small_gap:.1f}/{big_gap:.1f} "
            f"阈值={threshold:.1f}")

        return threshold

    def _detect_page_header_bottom(self, page) -> float:
        """检测当前页的表头底部Y坐标。

        后续页可能在不同Y坐标重复表头。通过匹配已知列名来检测。

        Args:
            page: PDF页面对象

        Returns:
            表头底部Y坐标，如果未检测到表头则返回 0.0
        """
        if not self.col_names:
            return 0.0

        wds = page.extract_words(x_tolerance=3)
        if not wds:
            return 0.0

        rows = group_by_y(wds, tol=5)
        # 用已知列名匹配当前页的表头行
        for i, row in enumerate(rows):
            line_text = ' '.join(w['text'] for w in row)
            # 检查是否有至少3个列名出现在此行中
            match_count = sum(
                1 for cn in self.col_names if cn and cn in line_text)
            if match_count >= 3:
                bottom = max(w['bottom'] for w in row)
                # 向下扫描英文翻译行
                scan = i + 1
                while scan < len(rows):
                    next_text = ' '.join(w['text'] for w in rows[scan])
                    plain = next_text.replace(' ', '')
                    if plain and re.match(r'^[A-Za-z]+$', plain):
                        bottom = max(
                            w['bottom'] for w in rows[scan])
                        scan += 1
                    else:
                        break
                return bottom
        return 0.0

    def _chars_of(self, page):
        """提取页面中所有有效字符的坐标信息。"""
        try:
            return [{
                'text': c.get('text', ''),
                'x0': c.get('x0', 0),
                'x1': c.get('x1', 0),
                'top': c.get('top', 0),
                'bottom': c.get('bottom', 0),
            } for c in page.chars if c.get('text', '').strip()]
        except Exception:
            return []

    def _extract_meta(self, page, hdr_top):
        """提取表头上方的元数据（如户名、账号等）。"""
        wds = [w for w in page.extract_words() if w['top'] < hdr_top]
        res = []
        for row in group_by_y(wds, tol=5):
            line = ' '.join(w['text'] for w in row).strip()
            if not line or any(p.search(line) for p in UNIVERSAL_SKIP_PATTERNS):
                continue
            # 跳过纯英文行（中英双语表头的翻译行，如 "Name Account No"）
            plain = line.replace(' ', '')
            if plain and re.match(r'^[A-Za-z]+$', plain):
                continue

            parts = META_SEP.split(line)
            if len(parts) < 2:
                # 没有冒号，尝试用大空格拆分
                sub_parts = re.split(r'\s{3,}', line)
                if len(sub_parts) >= 2:
                    row_pairs = []
                    for sp in sub_parts:
                        kv = META_SEP.split(sp)
                        if len(kv) >= 2:
                            row_pairs.append(
                                (kv[0].strip(), kv[1].strip()))
                        else:
                            row_pairs.append(('', sp.strip()))
                    res.append(row_pairs)
                else:
                    res.append([('', line)])
                continue

            pairs, key = [], parts[0].strip()
            for i, chunk in enumerate(parts[1:], 1):
                chunk = chunk.strip()
                if i < len(parts) - 1:
                    toks = chunk.split()
                    if toks:
                        # 从末尾向前收集连续的单个中文字符 token，
                        # 拼接成完整键名。
                        # 例：["ALL/全币种", "开", "户", "行"] → key="开户行"
                        # 例：["2026-04-10", "22:28:09", "验", "证", "码"] → key="验证码"
                        trail_start = len(toks)
                        for j in range(len(toks) - 1, -1, -1):
                            if len(toks[j]) == 1 and ord(toks[j]) > 0x2E7F:
                                trail_start = j
                            else:
                                break
                        if trail_start < len(toks):
                            next_key = ''.join(toks[trail_start:])
                            pairs.append(
                                (key, ' '.join(toks[:trail_start])))
                            key = next_key
                        else:
                            pairs.append(
                                (key, ' '.join(toks[:-1])))
                            key = toks[-1]
                    else:
                        pairs.append((key, ''))
                else:
                    pairs.append((key, chunk))
            if pairs:
                res.append(pairs)
        # 统一清洗键值，防止 PDF 中的控制字符导致 Excel 写入失败
        # （兴业银行等 PDF 的字体映射会产生 \x01 等非法字符）
        return [[(sanitize(k), sanitize(v)) for k, v in pairs]
                for pairs in res]

    def _find_header(self, row_groups):
        """
        通用表头检测：用 UNIVERSAL_HEADER_KW 对每行打分，
        得分最高的行即为表头。
        支持多行换行表头：通过 x0 坐标对齐来判断相邻行是否属于同一表头。
        """
        scores = []
        for g in row_groups:
            line_text = ' '.join(w['text'] for w in g)
            score = sum(1 for k in UNIVERSAL_HEADER_KW if k in line_text)
            scores.append(score)

        best = -1
        max_s = 0
        for i, s in enumerate(scores):
            if s > max_s:
                max_s, best = s, i
        # 至少命中 3 个关键词才认为是表头
        if best < 0 or max_s < 3:
            return -1, -1

        # 收集已确认表头行中所有词的 x0 坐标集合
        confirmed_x0s = {round(w['x0'], 0) for w in row_groups[best]}

        def _is_header_continuation(candidate_group, confirmed_x0_set):
            """判断候选行是否是表头的续行（基于 x0 坐标对齐）。

            对于 score=0 的行采用严格判断：必须是短文字片段（如"水号""期"
            "通号""号"等），不能包含数字/字母（那是数据行），且所有词都
            必须与已确认表头的 x0 对齐。

            Args:
                candidate_group: 候选行的 word 列表
                confirmed_x0_set: 已确认表头行的 x0 坐标集合

            Returns:
                如果候选行中有词的 x0 与已确认表头对齐，返回 True
            """
            candidate_text = ' '.join(w['text'] for w in candidate_group)
            # 含冒号的行是元数据，不算表头
            if '：' in candidate_text or ':' in candidate_text:
                return False
            # 如果 score >= 1 直接算续行
            cand_score = sum(
                1 for k in UNIVERSAL_HEADER_KW if k in candidate_text)
            if cand_score >= 1:
                return True
            # score=0 时采用严格模式：
            # 条件1: 词数量要少（表头碎片通常1-4个词）
            if len(candidate_group) > 5:
                return False
            # 条件2: 不含数字和ASCII字母（纯中文表头碎片）
            plain = candidate_text.replace(' ', '')
            if re.search(r'[0-9a-zA-Z]', plain):
                return False
            # 条件3: 每个词都必须与已确认表头的某个 x0 对齐
            aligned_count = 0
            for w in candidate_group:
                wx0 = round(w['x0'], 0)
                for cx0 in confirmed_x0_set:
                    if abs(wx0 - cx0) < 15:
                        aligned_count += 1
                        break
            # 所有词都对齐才算续行
            return aligned_count == len(candidate_group)

        # 尝试向上下扩展（多行表头）
        start, end = best, best
        while (start > 0
               and (row_groups[start][0]['top']
                    - row_groups[start - 1][0]['top']) < 25):
            if _is_header_continuation(
                    row_groups[start - 1], confirmed_x0s):
                # 将新行的 x0 也加入已确认集合
                for w in row_groups[start - 1]:
                    confirmed_x0s.add(round(w['x0'], 0))
                start -= 1
            else:
                break
        while (end < len(row_groups) - 1
               and (row_groups[end + 1][0]['top']
                    - row_groups[end][0]['top']) < 25):
            if _is_header_continuation(
                    row_groups[end + 1], confirmed_x0s):
                for w in row_groups[end + 1]:
                    confirmed_x0s.add(round(w['x0'], 0))
                end += 1
            else:
                break

        # 检测紧跟表头的英文翻译行（如招商银行 PDF 中的
        # "Date Currency Transaction Amount Balance ..."）。
        # 这些行不参与列名构建，但需要更新 hdr_bottom 来跳过它们。
        extra_bottom = 0.0
        scan = end + 1
        while (scan < len(row_groups)
               and (row_groups[scan][0]['top']
                    - row_groups[max(scan - 1, end)][0]['top']) < 25):
            candidate_text = ' '.join(
                w['text'] for w in row_groups[scan])
            # 纯英文/空格行（不含中文、数字或冒号）
            plain = candidate_text.replace(' ', '')
            is_english_only = (
                plain
                and re.match(r'^[A-Za-z]+$', plain)
            )
            if is_english_only:
                # 检查至少有一个词与表头 x0 对齐
                has_align = any(
                    any(abs(round(w['x0'], 0) - cx) < 15
                        for cx in confirmed_x0s)
                    for w in row_groups[scan]
                )
                if has_align:
                    extra_bottom = max(
                        w['bottom'] for w in row_groups[scan])
                    scan += 1
                    continue
            break

        return start, end, extra_bottom

    def _is_header_row(self, row):
        """判断一行数据是否是重复的表头行。"""
        if not self.col_names:
            return False
        n = min(len(row), len(self.col_names))
        if n == 0:
            return False
        return all(sanitize(row[i]) == self.col_names[i] for i in range(n))

    def _merge_cont(self, rows):
        """智能合并：如果一行的首列（通常是日期）为空，则合并到上一行。"""
        if not rows:
            return rows
        out = [rows[0]]
        for row in rows[1:]:
            # 首列为空 + 仅少量列有内容 -> 视为上一行的续行
            is_empty_first = not row[0].strip() if row else True
            content_cols = [i for i, c in enumerate(row) if c.strip()]

            if (is_empty_first
                    and 0 < len(content_cols) < max(2, len(row) // 2)):
                for i in content_cols:
                    if i < len(out[-1]):
                        sep = " " if out[-1][i] else ""
                        out[-1][i] = out[-1][i] + sep + row[i]
            else:
                out.append(row)
        return out

    def _postprocess_dates(self):
        """后处理：规范化日期列的格式。

        多行合并后日期可能呈现碎片化格式（如 '2024-0 1-021 7:04:48'），
        本方法自动检测含日期的列，并将其规范化为 'YYYY-MM-DD HH:MM:SS'。

        处理模式：
        1. 跨行拼接碎片：'2024-0 1-021 7:04:48' -> '2024-01-02 7:04:48'
        2. 通用日期空格修复：'2024 -01 -02' -> '2024-01-02'
        3. 缺年份补全（跨页断行）
        """
        if not self.data_rows:
            return

        # 通用日期碎片修复正则
        # 模式: YYYY-M₁ M₂-DD[FLAG] H:MM:SS
        # 例: '2024-0 1-021 7:04:48' -> year=2024, m1=0, m2=1, dd=02, flag=1
        _FRAG_DATE_RE = re.compile(
            r'(\d{4})-(\d)\s+(\d)-(\d{2})'
            r'([01])\s+'
            r'(\d{1,2}:\d{2}:\d{2})'
        )
        # 缺年份的碎片: '1-091 9:45:56'
        _FRAG_DATE_NOYEAR_RE = re.compile(
            r'^(\d)-(\d{2})'
            r'([01])\s+'
            r'(\d{1,2}:\d{2}:\d{2})$'
        )

        def fix_date(val: str) -> str:
            """尝试修复单个日期值。

            Args:
                val: 原始日期文本

            Returns:
                修复后的日期文本
            """
            if not val:
                return val

            def _pad_time(t: str) -> str:
                """将时间 H:MM:SS 补零为 HH:MM:SS。"""
                parts = t.split(':')
                if len(parts) >= 2:
                    parts[0] = parts[0].zfill(2)
                return ':'.join(parts)

            # 模式1: 完整年份碎片日期
            m = _FRAG_DATE_RE.search(val)
            if m:
                year, m1, m2, dd, _flag, time = m.groups()
                month = f'{m1}{m2}'
                return f'{year}-{month.zfill(2)}-{dd} {_pad_time(time)}'
            # 模式2: 缺年份碎片日期（跨页断行）
            m = _FRAG_DATE_NOYEAR_RE.search(val)
            if m:
                m2, dd, _flag, time = m.groups()
                return f'-{m2.zfill(2)}-{dd} {_pad_time(time)}'
            return val

        # 自动检测日期列：列名含'日期'/'时间'/'date'/'time'
        date_col_indices = []
        for i, name in enumerate(self.col_names):
            lower = name.lower()
            if any(kw in lower for kw in ['日期', '时间', 'date', 'time']):
                date_col_indices.append(i)

        # 如果没找到，尝试通过内容启发式检测（支持多种日期格式）
        if not date_col_indices and len(self.data_rows) > 0:
            for i in range(len(self.col_names)):
                sample = [r[i] for r in self.data_rows[:20]
                          if i < len(r) and r[i]]
                if sample and sum(
                        1 for s in sample
                        if DATE_CONTENT_RE.search(s)) > len(sample) * 0.5:
                    date_col_indices.append(i)

        if not date_col_indices:
            return

        for row in self.data_rows:
            for ci in date_col_indices:
                if ci < len(row) and row[ci]:
                    row[ci] = fix_date(row[ci])

        # 第二步：填充缺失的年份（从相邻记录推断）
        for ci in date_col_indices:
            last_year = None
            for row in self.data_rows:
                if ci >= len(row) or not row[ci]:
                    continue
                # 提取年份（兼容 -、/、.、8位无分隔、中文"年"等格式）
                year = extract_year(row[ci])
                if year:
                    last_year = year
                elif row[ci].startswith('-') and last_year:
                    # 缺年份，用上一条的年份填充
                    row[ci] = last_year + row[ci]

    def _merge_cross_page(self, rows: list) -> list:
        """合并跨页断行的记录。

        跨页断行时，一条记录的前半段（通常只有1行，很多列为空）在上一页末尾，
        后半段（通常2行，某些列为空）在下一页开头。

        检测标志：
        - 前半段：很多列为空（只有1行数据，大部分列没内容）
        - 后半段：日期列缺年份（以 M-DD 或 -MM-DD 开头）

        Args:
            rows: 数据行列表

        Returns:
            合并后的数据行列表
        """
        if not rows or len(rows) < 2:
            return rows

        # 找到日期列索引
        date_ci = -1
        for i, name in enumerate(self.col_names):
            if any(kw in name.lower() for kw in ['日期', '时间', 'date', 'time']):
                date_ci = i
                break

        out = []
        i = 0
        while i < len(rows):
            row = rows[i]
            # 检查下一行是否是跨页后半段
            if (i + 1 < len(rows)
                    and date_ci >= 0 and date_ci < len(row)
                    and date_ci < len(rows[i + 1])):
                next_row = rows[i + 1]
                cur_date = row[date_ci].strip()
                next_date = next_row[date_ci].strip()

                # 当前行日期不完整（只有年份开头或很短）
                # 且下一行日期缺年份（不以完整日期开头，但含类似 M-DD 的碎片）
                cur_empty_count = sum(1 for c in row if not c.strip())
                next_incomplete = (
                    next_date
                    and not starts_with_date(next_date)
                    and re.search(r'\d+[-‑–/.]\d+', next_date)
                )

                if (cur_empty_count > len(row) // 2 and next_incomplete):
                    # 合并：将两行的非空内容拼接
                    merged = []
                    for j in range(max(len(row), len(next_row))):
                        v1 = row[j].strip() if j < len(row) else ''
                        v2 = next_row[j].strip() if j < len(next_row) else ''
                        if v1 and v2:
                            merged.append(f'{v1} {v2}')
                        else:
                            merged.append(v1 or v2)
                    out.append(merged)
                    i += 2  # 跳过两行
                    continue

            out.append(row)
            i += 1
        return out

    def _postprocess_incomplete_records(self):
        """修复跨页导致的不完整记录。

        跨页断行时，记录首行（含前缀如 C0646R）丢失，导致流水号和
        打印实例号缺少前缀段。

        算法：
        1. 检测日期列，找出被年份填充过的行（跨页标记行）
        2. 对标记行的指定列，从相邻完整行推断前缀并补全
        """
        if not self.data_rows or len(self.data_rows) < 5:
            return

        # 找日期列
        date_ci = -1
        for i, name in enumerate(self.col_names):
            if any(kw in name.lower() for kw in ['日期', '时间', 'date', 'time']):
                date_ci = i
                break

        # 找需要补全的列
        check_cols = []
        for i, name in enumerate(self.col_names):
            lower = name.lower()
            if any(kw in lower for kw in
                   ['流水', '实例', '编号', '序号']):
                check_cols.append(i)
        if not check_cols and self.col_names:
            check_cols = [0]

        # 识别跨页行：该行的列值比相邻行少一段（缺少前缀段）
        for ci in check_cols:
            for idx, row in enumerate(self.data_rows):
                if ci >= len(row) or not row[ci].strip():
                    continue

                cur_val = row[ci].strip()
                cur_parts = cur_val.split()

                # 获取相邻行（优先下一行，其次上一行）的前缀
                ref_val = None
                for ref_idx in [idx + 1, idx - 1]:
                    if 0 <= ref_idx < len(self.data_rows):
                        ref_row = self.data_rows[ref_idx]
                        if ci < len(ref_row) and ref_row[ci].strip():
                            ref_val = ref_row[ci].strip()
                            break

                if not ref_val:
                    continue

                ref_parts = ref_val.split()
                if len(ref_parts) <= 1:
                    continue

                ref_prefix = ref_parts[0]

                # 条件：当前行比参考行少一段，且当前首段不像前缀
                if (len(cur_parts) == len(ref_parts) - 1
                        and cur_parts[0] != ref_prefix):
                    row[ci] = f'{ref_prefix} {cur_val}'



# ═══════════════════════════════════════════════════════
# Excel 输出
# ═══════════════════════════════════════════════════════

def write_excel(p: BankPDFParser, out: str):
    wb = openpyxl.Workbook()
    ws = wb.active
    ws.title = '流水明细'
    r = 1

    # 样式
    f_meta_k = Font(name='微软雅黑', size=9, bold=True, color='1F3864')
    f_meta_v = Font(name='微软雅黑', size=9)
    fill_meta_k = PatternFill('solid', fgColor=C_MK_BG)
    f_hdr = Font(name='微软雅黑', size=10, bold=True, color=C_HDR_FG)
    fill_hdr = PatternFill('solid', fgColor=C_HDR_BG)
    bd = Border(
        left=Side(style='thin', color=C_BDR),
        right=Side(style='thin', color=C_BDR),
        top=Side(style='thin', color=C_BDR),
        bottom=Side(style='thin', color=C_BDR),
    )

    # 元数据
    for pairs in p.metadata:
        c = 1
        for k, v in pairs:
            if k:
                cell = ws.cell(r, c, k + '：')
                cell.font, cell.fill, cell.border = f_meta_k, fill_meta_k, bd
                c += 1
            cell = ws.cell(r, c, v)
            cell.font, cell.border = f_meta_v, bd
            c += 1
        r += 1
    if p.metadata:
        r += 1

    # 表头
    h_row = r
    for ci, name in enumerate(p.col_names, 1):
        cell = ws.cell(r, ci, name)
        cell.font = f_hdr
        cell.fill = fill_hdr
        cell.alignment = Alignment(horizontal='center', vertical='center')
        cell.border = Border(
            left=Side(style='thin', color='FFFFFF'),
            right=Side(style='thin', color='FFFFFF'),
        )
    r += 1

    # 仅转换明确的金额列；账号和凭证号必须保留文本，避免长数字丢失精度。
    amount_headers = {'收入', '支出', '余额', '账户余额', '交易金额', '发生额',
                      '借方发生额', '贷方发生额', '借方金额', '贷方金额'}
    amount_columns = {i for i, name in enumerate(p.col_names, 1)
                      if re.sub(r'\s+', '', name) in amount_headers}

    # 数据行
    f_data = Font(name='微软雅黑', size=9)
    fill_alt = PatternFill('solid', fgColor=C_ALT)
    for ri, row in enumerate(p.data_rows):
        fill = fill_alt if ri % 2 else None
        for ci, v in enumerate(row, 1):
            cell = ws.cell(r, ci, v)
            if ci in amount_columns and isinstance(v, str):
                text = v.strip()
                # 无法明确识别的内容保持原样，不将空值或 OCR 异常伪装成零。
                if re.fullmatch(r'[+-]?(?:\d+|\d{1,3}(?:,\d{3})+)(?:\.\d{1,2})?', text):
                    digits = text.replace(',', '').lstrip('+-').replace('.', '').lstrip('0')
                    if len(digits) <= 15:
                        cell.value = float(text.replace(',', ''))
                        cell.number_format = '#,##0.00'
            cell.font = f_data
            cell.alignment = Alignment(vertical='center', wrap_text=True)
            cell.border = bd
            if fill:
                cell.fill = fill
        r += 1

    # 列宽自适应
    for col in ws.columns:
        mw = 0
        for cell in col:
            if cell.value:
                mw = max(mw, sum(
                    2 if ord(c) > 127 else 1 for c in str(cell.value)))
        ws.column_dimensions[get_column_letter(col[0].column)].width = min(
            max(mw + 2, 8), 45)

    # 冻结表头
    if p.col_names:
        ws.freeze_panes = ws.cell(h_row + 1, 1)

    wb.save(out)
    logger.info(f"保存 Excel: {out}")


# ═══════════════════════════════════════════════════════
# 入口函数
# ═══════════════════════════════════════════════════════

def process_one(pdf_path: str, out_dir: str = None,
                template_path: str = None,
                progress_callback: Optional[Callable[[int, int], None]] = None,
                cancel_check: Optional[Callable[[], bool]] = None,
                password: Optional[str] = None) -> bool:
    """处理单个 PDF 文件。

    Args:
        pdf_path: PDF 文件路径
        out_dir: 输出目录，None 则与源文件同目录
        template_path: 保留接口兼容但不再使用
        progress_callback: 页面进度回调函数 (当前页, 总页数)
        cancel_check: 取消检查回调，返回 True 表示用户已取消
        password: 加密 PDF 的打开密码

    Returns:
        是否处理成功
    """
    try:
        p = Path(pdf_path)
        if not p.exists():
            logger.error(f"文件不存在: {p}")
            return False
        od = Path(out_dir) if out_dir else p.parent
        od.mkdir(parents=True, exist_ok=True)
        out = str(od / (p.stem + '.xlsx'))
        parser = BankPDFParser(
            str(p),
            password=password,
            progress_callback=progress_callback,
            cancel_check=cancel_check,
        ).parse()
        write_excel(parser, out)
        return True
    except Exception as e:
        if _is_encryption_error(e):
            logger.error(
                f"处理失败 {Path(pdf_path).name}: "
                f"PDF 已加密，未提供密码或密码不正确（可通过 -p 参数指定密码）")
            return False
        logger.error(f"处理失败 {Path(pdf_path).name}: {e}")
        import traceback
        logger.error(traceback.format_exc())
        return False


def main():
    ap = argparse.ArgumentParser(description='银行流水 PDF -> Excel 通用转换器')
    ap.add_argument('input', help='PDF 文件或目录路径')
    ap.add_argument('-o', '--output', default=None, help='输出目录')
    ap.add_argument('-r', '--recursive', action='store_true',
                    help='递归处理子目录')
    ap.add_argument('-p', '--password', default=None,
                    help='加密 PDF 的打开密码（批量模式下应用于所有文件）')
    args = ap.parse_args()
    ip = Path(args.input)
    if ip.is_file():
        ok = process_one(str(ip), args.output, password=args.password)
        sys.exit(0 if ok else 1)
    elif ip.is_dir():
        pdfs = list(ip.glob('**/*.pdf' if args.recursive else '*.pdf'))
        ok = fail = 0
        for f in pdfs:
            if process_one(str(f), args.output, password=args.password):
                ok += 1
            else:
                fail += 1
        logger.info(f'批量完成: 成功 {ok}, 失败 {fail}')
        sys.exit(0 if fail == 0 else 1)
    else:
        logger.error(f'路径不存在: {ip}')
        sys.exit(1)


if __name__ == '__main__':
    main()
