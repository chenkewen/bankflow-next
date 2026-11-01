"""
OCR 银行流水 (扫描件 PDF / 图片) -> Excel 智能转换引擎 (v1.0)
与 pdf_to_excel.py 平行独立运行，不干扰原有电子版 PDF 解析逻辑，
仅复用其经过验证的公共分词聚类、表头评分、日期规范化及 write_excel 输出函数。

核心能力：
1. 支持扫描件 PDF（支持密码解密）及主流图片格式 (.png/.jpg/.jpeg/.bmp/.tiff/.webp)
2. 像素级红章剥离（区分白底红章与黑字重叠红章，还原印章下覆盖的黑字）+ 斜向水印框几何过滤
3. RapidOCR (PP-OCRv4 ONNX) 单线程推理，先剔除斜向水印并复用完全相同的文字块
4. 数学坐标级自动旋转纠偏（Deskew），解决扫描/拍照倾斜导致的跨列串行问题
5. 金融级数字形近字纠偏 + 跨行时间斷裂修复 + 借贷发生额与余额钩稽公式自动核验与修复
"""

import re
import os
import sys
import math
import hashlib
import logging
import argparse
from pathlib import Path
from typing import Optional, List, Tuple, Dict, Callable

import types
import numpy as np
from PIL import Image, ImageOps, ImageDraw
import pypdfium2 as pdfium


def _install_lightweight_cv2_and_shapely() -> None:
    """用基于 NumPy + Pillow 的轻量级实现替代 82MB 的 cv2.pyd 与 3.8MB 的 shapely。"""
    if 'cv2' not in sys.modules:
        cv2_mod = types.ModuleType('cv2')
        cv2_mod.COLOR_GRAY2BGR = 8
        cv2_mod.COLOR_RGB2BGR = 4
        cv2_mod.COLOR_BGR2RGB = 4
        cv2_mod.BORDER_CONSTANT = 0
        cv2_mod.BORDER_REPLICATE = 1
        cv2_mod.INTER_LINEAR = 1
        cv2_mod.INTER_CUBIC = 2
        cv2_mod.RETR_LIST = 1
        cv2_mod.CHAIN_APPROX_SIMPLE = 2
        cv2_mod.ROTATE_90_CLOCKWISE = 0
        cv2_mod.ROTATE_180 = 1
        cv2_mod.ROTATE_90_COUNTERCLOCKWISE = 2
        cv2_mod.FONT_HERSHEY_SIMPLEX = 0

        def _cvtColor(img: np.ndarray, code: int) -> np.ndarray:
            if code == 8:
                if img.ndim == 2:
                    return np.repeat(img[:, :, None], 3, axis=2)
                if img.ndim == 3 and img.shape[2] == 1:
                    return np.repeat(img, 3, axis=2)
                return img
            return np.ascontiguousarray(img[..., ::-1])

        def _resize(img: np.ndarray, dsize: Tuple[int, int], interpolation: int = 1) -> np.ndarray:
            w, h = int(dsize[0]), int(dsize[1])
            resample = Image.Resampling.BICUBIC if interpolation == 2 else Image.Resampling.BILINEAR
            return np.asarray(Image.fromarray(img).resize((w, h), resample=resample))

        def _copyMakeBorder(img: np.ndarray, top: int, bottom: int, left: int, right: int,
                            borderType: int = 0, value: object = 0) -> np.ndarray:
            top, bottom, left, right = int(top), int(bottom), int(left), int(right)
            if borderType == 0:
                h, w = img.shape[:2]
                out_shape = (h + top + bottom, w + left + right) + img.shape[2:]
                out = np.full(out_shape, value, dtype=img.dtype)
                out[top:top + h, left:left + w] = img
                return out
            pad_w = ((top, bottom), (left, right)) + ((0, 0),) * (img.ndim - 2)
            return np.pad(img, pad_w, mode='edge')

        def _rotate(img: np.ndarray, code: int) -> np.ndarray:
            if code == 1:
                return np.ascontiguousarray(np.rot90(img, 2))
            if code == 0:
                return np.ascontiguousarray(np.rot90(img, -1))
            return np.ascontiguousarray(np.rot90(img, 1))

        def _dilate(src: np.ndarray, kernel: np.ndarray) -> np.ndarray:
            kh, kw = kernel.shape[:2]
            h, w = src.shape[:2]
            padded = np.pad(src, ((0, kh - 1), (0, kw - 1)), mode='constant', constant_values=0)
            out = np.zeros_like(src)
            for dy in range(kh):
                for dx in range(kw):
                    if kernel[dy, dx]:
                        out = np.maximum(out, padded[dy:dy + h, dx:dx + w])
            return out

        def _findContours(mask_u8: np.ndarray, mode: int, method: int):
            m = (mask_u8 > 0)
            h, w = m.shape
            padded = np.zeros((h, w + 2), dtype=np.int8)
            padded[:, 1:-1] = m
            diff = np.diff(padded, axis=1)
            ys, x0s = np.nonzero(diff == 1)
            _, x1s = np.nonzero(diff == -1)
            n_runs = len(ys)
            if n_runs == 0:
                return [], None
            parent = list(range(n_runs))
            row_splits = np.flatnonzero(np.diff(ys)) + 1
            row_indices = np.split(np.arange(n_runs), row_splits)
            row_ys = ys[np.r_[0, row_splits]]
            prev_y = -2
            prev_runs = []
            for ry, r_idxs in zip(row_ys, row_indices):
                if ry == prev_y + 1:
                    i_prev = 0
                    n_prev = len(prev_runs)
                    for idx in r_idxs:
                        cx0, cx1 = x0s[idx] - 1, x1s[idx]
                        while i_prev < n_prev and x1s[prev_runs[i_prev]] <= cx0:
                            i_prev += 1
                        j = i_prev
                        while j < n_prev and x0s[prev_runs[j]] <= cx1:
                            ra, rb = int(idx), int(prev_runs[j])
                            while parent[ra] != ra:
                                parent[ra] = parent[parent[ra]]
                                ra = parent[ra]
                            while parent[rb] != rb:
                                parent[rb] = parent[parent[rb]]
                                rb = parent[rb]
                            if ra != rb:
                                if ra < rb:
                                    parent[rb] = ra
                                else:
                                    parent[ra] = rb
                            j += 1
                prev_y = ry
                prev_runs = r_idxs
            comps: Dict[int, List[int]] = {}
            for i in range(n_runs):
                r = i
                while parent[r] != r:
                    parent[r] = parent[parent[r]]
                    r = parent[r]
                if r not in comps:
                    comps[r] = []
                comps[r].append(i)
            contours = []
            for idxs_list in comps.values():
                idxs = np.array(idxs_list)
                ry = ys[idxs]
                rx0 = x0s[idxs]
                rx1 = x1s[idxs] - 1
                pts = np.empty((len(idxs) * 2, 1, 2), dtype=np.int32)
                pts[0::2, 0, 0] = rx0
                pts[0::2, 0, 1] = ry
                pts[1::2, 0, 0] = rx1
                pts[1::2, 0, 1] = ry
                contours.append(pts)
            return contours, None

        def _minAreaRect(contour: np.ndarray):
            pts = np.asarray(contour, dtype=np.float32).reshape(-1, 2)
            n = len(pts)
            if n == 0:
                return ((0.0, 0.0), (0.0, 0.0), 0.0)
            if n <= 2:
                x0, y0 = pts.min(axis=0)
                x1, y1 = pts.max(axis=0)
                return ((float(x0 + x1) * 0.5, float(y0 + y1) * 0.5),
                        (float(np.hypot(x1 - x0, y1 - y0)), 0.0), 0.0)
            if n > 4:
                order = np.lexsort((pts[:, 1], pts[:, 0]))
                spts = pts[order]
                lower = []
                for p in spts:
                    while len(lower) >= 2 and (
                        (lower[-1][0] - lower[-2][0]) * (p[1] - lower[-2][1])
                        - (lower[-1][1] - lower[-2][1]) * (p[0] - lower[-2][0])
                    ) <= 0:
                        lower.pop()
                    lower.append(p)
                upper = []
                for p in spts[::-1]:
                    while len(upper) >= 2 and (
                        (upper[-1][0] - upper[-2][0]) * (p[1] - upper[-2][1])
                        - (upper[-1][1] - upper[-2][1]) * (p[0] - upper[-2][0])
                    ) <= 0:
                        upper.pop()
                    upper.append(p)
                hull = np.array(lower[:-1] + upper[:-1], dtype=np.float32)
            else:
                hull = pts
            if len(hull) <= 2:
                x0, y0 = pts.min(axis=0)
                x1, y1 = pts.max(axis=0)
                return ((float(x0 + x1) * 0.5, float(y0 + y1) * 0.5),
                        (float(np.hypot(x1 - x0, y1 - y0)), 0.0), 0.0)
            edges = np.roll(hull, -1, axis=0) - hull
            lengths = np.hypot(edges[:, 0], edges[:, 1])
            valid = lengths > 1e-6
            if not np.any(valid):
                x0, y0 = pts.min(axis=0)
                x1, y1 = pts.max(axis=0)
                return ((float(x0 + x1) * 0.5, float(y0 + y1) * 0.5),
                        (float(x1 - x0), float(y1 - y0)), 0.0)
            edges = edges[valid]
            lengths = lengths[valid]
            cos_a = edges[:, 0] / lengths
            sin_a = edges[:, 1] / lengths
            proj_x = cos_a[:, None] * hull[None, :, 0] + sin_a[:, None] * hull[None, :, 1]
            proj_y = -sin_a[:, None] * hull[None, :, 0] + cos_a[:, None] * hull[None, :, 1]
            min_x, max_x = proj_x.min(axis=1), proj_x.max(axis=1)
            min_y, max_y = proj_y.min(axis=1), proj_y.max(axis=1)
            widths, heights = max_x - min_x, max_y - min_y
            best = int(np.argmin(widths * heights))
            c, s = float(cos_a[best]), float(sin_a[best])
            w, h = float(widths[best]), float(heights[best])
            cx_p = float(min_x[best] + max_x[best]) * 0.5
            cy_p = float(min_y[best] + max_y[best]) * 0.5
            cx = c * cx_p - s * cy_p
            cy = s * cx_p + c * cy_p
            angle = float(np.degrees(np.arctan2(s, c)))
            return ((cx, cy), (w, h), angle)

        def _mat_mul(A: np.ndarray, B: np.ndarray) -> np.ndarray:
            return (A[:, :, None] * B[None, :, :]).sum(axis=1)

        def _gauss_solve(A: np.ndarray, B: np.ndarray) -> np.ndarray:
            """纯 NumPy 高斯列主元消元求解 A * X = B（免除对 _umath_linalg / OpenBLAS 的重型依赖）。"""
            A = np.array(A, dtype=np.float64, copy=True)
            B_arr = np.array(B, dtype=np.float64, copy=True)
            is_1d = (B_arr.ndim == 1)
            if is_1d:
                B_arr = B_arr[:, None]
            n = A.shape[0]
            for i in range(n):
                piv = i + int(np.argmax(np.abs(A[i:, i])))
                if piv != i:
                    A[[i, piv]] = A[[piv, i]]
                    B_arr[[i, piv]] = B_arr[[piv, i]]
                diag = A[i, i]
                if abs(diag) < 1e-15:
                    raise ValueError("Singular matrix")
                A[i] /= diag
                B_arr[i] /= diag
                for k in range(n):
                    if k != i:
                        factor = A[k, i]
                        if factor != 0.0:
                            A[k] -= factor * A[i]
                            B_arr[k] -= factor * B_arr[i]
            return B_arr[:, 0] if is_1d else B_arr

        def _inv_mat(M: np.ndarray) -> np.ndarray:
            return _gauss_solve(M, np.eye(M.shape[0], dtype=np.float64))

        def _boxPoints(rect) -> np.ndarray:
            (cx, cy), (w, h), angle = rect
            theta = np.radians(angle)
            c, s = float(np.cos(theta)), float(np.sin(theta))
            dx, dy = w * 0.5, h * 0.5
            corners = np.array([[-dx, -dy], [dx, -dy], [dx, dy], [-dx, dy]], dtype=np.float32)
            rot = np.array([[c, s], [-s, c]], dtype=np.float32)
            return _mat_mul(corners, rot) + np.array([cx, cy], dtype=np.float32)

        def _fillPoly(mask: np.ndarray, polys: np.ndarray, color: int) -> np.ndarray:
            pil_m = Image.fromarray(mask)
            draw = ImageDraw.Draw(pil_m)
            pts_arr = np.asarray(polys)
            if pts_arr.ndim == 2:
                pts_arr = pts_arr[None, :, :]
            for poly in pts_arr:
                pts_list = [tuple(map(int, p)) for p in poly.reshape(-1, 2)]
                if len(pts_list) >= 3:
                    draw.polygon(pts_list, fill=int(color))
            mask[:] = np.asarray(pil_m)
            return mask

        def _mean(src: np.ndarray, mask: Optional[np.ndarray] = None):
            if mask is not None:
                m = mask > 0
                val = float(src[m].mean()) if np.any(m) else 0.0
            else:
                val = float(src.mean())
            return (val, 0.0, 0.0, 0.0)

        def _getPerspectiveTransform(src: np.ndarray, dst: np.ndarray) -> np.ndarray:
            src = np.asarray(src, dtype=np.float64).reshape(4, 2)
            dst = np.asarray(dst, dtype=np.float64).reshape(4, 2)
            A = np.zeros((8, 8), dtype=np.float64)
            b = np.zeros(8, dtype=np.float64)
            for i in range(4):
                x, y = src[i]
                u, v = dst[i]
                A[2 * i] = [x, y, 1, 0, 0, 0, -u * x, -u * y]
                A[2 * i + 1] = [0, 0, 0, x, y, 1, -v * x, -v * y]
                b[2 * i] = u
                b[2 * i + 1] = v
            h = _gauss_solve(A, b)
            return np.append(h, 1.0).reshape(3, 3)

        def _invert(M: np.ndarray):
            try:
                return True, _inv_mat(M)
            except Exception:
                return False, np.zeros_like(M)

        def _warpPerspective(img: np.ndarray, M: np.ndarray, dsize: Tuple[int, int],
                             borderMode: int = 1, flags: int = 2) -> np.ndarray:
            w, h = int(dsize[0]), int(dsize[1])
            M_inv = _inv_mat(M)
            dst_corners = np.array([[0, w, w, 0], [0, 0, h, h], [1, 1, 1, 1]], dtype=np.float64)
            src_corners = _mat_mul(M_inv, dst_corners)
            src_corners /= src_corners[2:3, :]
            ih, iw = img.shape[:2]
            ix0 = int(np.floor(src_corners[0].min())) - 2
            iy0 = int(np.floor(src_corners[1].min())) - 2
            ix1 = int(np.ceil(src_corners[0].max())) + 3
            iy1 = int(np.ceil(src_corners[1].max())) + 3
            if 0 <= ix0 and 0 <= iy0 and ix1 <= iw and iy1 <= ih:
                patch = img[iy0:iy1, ix0:ix1]
            else:
                cx0, cy0 = max(0, ix0), max(0, iy0)
                cx1, cy1 = min(iw, ix1), min(ih, iy1)
                core = img[cy0:cy1, cx0:cx1]
                pt = max(0, cy0 - iy0)
                pb = max(0, iy1 - cy1)
                pl = max(0, cx0 - ix0)
                pr = max(0, ix1 - cx1)
                pad_w = ((pt, pb), (pl, pr)) + ((0, 0),) * (img.ndim - 2)
                patch = np.pad(core, pad_w, mode='edge')
            T_shift = np.array([[1.0, 0.0, -ix0], [0.0, 1.0, -iy0], [0.0, 0.0, 1.0]])
            T_pre = np.array([[1.0, 0.0, -0.5], [0.0, 1.0, -0.5], [0.0, 0.0, 1.0]])
            T_post = np.array([[1.0, 0.0, 0.5], [0.0, 1.0, 0.5], [0.0, 0.0, 1.0]])
            M_pil = _mat_mul(_mat_mul(_mat_mul(T_post, T_shift), M_inv), T_pre)
            M_pil /= M_pil[2, 2]
            coeffs = M_pil.ravel()[:8]
            resample = Image.Resampling.BICUBIC if flags == 2 else Image.Resampling.BILINEAR
            return np.asarray(Image.fromarray(patch).transform(
                (w, h), Image.Transform.PERSPECTIVE, coeffs, resample=resample))

        cv2_mod.cvtColor = _cvtColor
        cv2_mod.resize = _resize
        cv2_mod.copyMakeBorder = _copyMakeBorder
        cv2_mod.rotate = _rotate
        cv2_mod.dilate = _dilate
        cv2_mod.findContours = _findContours
        cv2_mod.minAreaRect = _minAreaRect
        cv2_mod.boxPoints = _boxPoints
        cv2_mod.fillPoly = _fillPoly
        cv2_mod.mean = _mean
        cv2_mod.getPerspectiveTransform = _getPerspectiveTransform
        cv2_mod.invert = _invert
        cv2_mod.warpPerspective = _warpPerspective
        sys.modules['cv2'] = cv2_mod

    if 'shapely' not in sys.modules:
        shapely_mod = types.ModuleType('shapely')
        shapely_geom_mod = types.ModuleType('shapely.geometry')

        class _Polygon:
            def __init__(self, pts):
                p = np.asarray(pts, dtype=np.float64).reshape(-1, 2)
                x, y = p[:, 0], p[:, 1]
                self.area = float(0.5 * abs((x * np.roll(y, -1)).sum() - (y * np.roll(x, -1)).sum()))
                edges = np.roll(p, -1, axis=0) - p
                self.length = max(float(np.hypot(edges[:, 0], edges[:, 1]).sum()), 1e-6)

        shapely_geom_mod.Polygon = _Polygon
        shapely_mod.geometry = shapely_geom_mod
        sys.modules['shapely'] = shapely_mod
        sys.modules['shapely.geometry'] = shapely_geom_mod


_install_lightweight_cv2_and_shapely()
import rapidocr_onnxruntime.main as _rapidocr_main
from rapidocr_onnxruntime import RapidOCR

from bankflow.parsers import pdf_to_excel
from bankflow.parsers.pdf_to_excel import (
    UNIVERSAL_HEADER_KW,
    UNIVERSAL_SKIP_PATTERNS,
    DATE_CONTENT_RE,
    starts_with_date,
    extract_year,
    sanitize,
    is_skip_row,
    drop_skip_lines,
    group_by_y,
    build_cols,
    chars_to_row,
    write_excel,
    _clean_col_name,
    _is_encryption_error,
)

# ─── 日志配置（共享 pdf_converter logger 以便 GUI 日志抽屉统一捕获） ───
logger = logging.getLogger('pdf_converter')

# 支持的图片扩展名
IMAGE_EXTENSIONS = {'.png', '.jpg', '.jpeg', '.bmp', '.tif', '.tiff', '.webp'}
OCR_SUPPORTED_EXTENSIONS = {'.pdf'} | IMAGE_EXTENSIONS

# 英文表头翻译行匹配（允许斜杠、撇号、点号、括号、连字符，如 Counterparty's Account No./）
_ENGLISH_HDR_LINE_RE = re.compile(r"^[A-Za-z/.'’()\-]+$")

# 常见印章/斜向水印干扰词过滤正则（注意：不可包含『股份有限公司』，否则会误删跨行银行/企业名称的尾行！）
_WATERMARK_TEXT_RE = re.compile(
    r'^(?:兴业银行\s*\d{4}[-/.]\d{1,2}(?:[-/.]\d{1,2})?(?:\s*\d{1,2}:\d{2}:\d{2})?'
    r'|电子回单|电子签名专用|业务专用章|会计专用章)$'
)

# 匹配原子级不可跨列切分的数字/日期/时间/账号片段
# 注意：带两位小数的金额必须精确匹配 \.\d{2}，以便在无空格粘连串（如 '0.001.329189.27'）中准确切开多个金额！
_ATOMIC_TOKEN_RE = re.compile(
    r'\d{4}[-‑–/.年]\d{1,2}[-‑–/.月]\d{1,2}日?'
    r'|\d{4}[-‑–/.]\d{1,2}[-‑–/.]'
    r'|\d{1,2}:\d{2}(?::\d{2})?'
    r'|[+-]?[¥￥$]?(?:\d{1,3}(?:,\d{3})+|\d+)\.\d{2}'
    r'|\d+'
    r'|[A-Za-z0-9_]+|[^\s]'
)

# 补充各大银行（工行、建行、农行、邮储、湖南银行、长沙银行、农商行、微信支付等）表头词
UNIVERSAL_HEADER_KW = set(UNIVERSAL_HEADER_KW) | {
    '转入金额', '转出金额', '收入金额', '支出金额', '借方发生额', '贷方发生额',
    '收入/支出金额', '借贷标志', '储种', '钞汇', '地区', '对方单位', '子账号',
    '交易余额', '外部系统流水', '交易单号', '商户单号', '交易对方', '交易方式',
    '金额(元)', '对方银行', '对方开户机构', '交易地点', '收支方向', '凭证代号',
}

# 按长度降序排列的表头关键词列表，用于拆分粘连在一起的连续表头框（如 '交易日期借方金额贷方金额余额'）
_SORTED_HDR_KWS = sorted(UNIVERSAL_HEADER_KW, key=len, reverse=True)


def split_concat_header_text(text: str) -> Optional[List[str]]:
    """若一个无空格文本块由多个已知表头关键词无缝拼接而成（如 '交易日期借方金额贷方金额余额'），
    将其切分为独立的表头词列表。仅当至少命中 2 个关键词且覆盖率 >= 85% 时生效。
    （注意：含斜杠 '/' 或括号 '()' 的复合单列名如 '收入/支出金额'、'借方发生额(支取)' 不切分！）
    """
    clean = text.strip()
    if (len(clean) < 4
            or ' ' in clean
            or any(c.isdigit() for c in clean)
            or any(c in clean for c in '/()（）')):
        return None
    parts = []
    idx = 0
    matched_chars = 0
    n = len(clean)
    while idx < n:
        hit = None
        for kw in _SORTED_HDR_KWS:
            if clean.startswith(kw, idx):
                hit = kw
                break
        if hit:
            parts.append(hit)
            idx += len(hit)
            matched_chars += len(hit)
        else:
            # 将单个未匹配字符并入前一个词或作为碎片
            if parts:
                parts[-1] += clean[idx]
            else:
                parts.append(clean[idx])
            idx += 1
    if len(parts) >= 2 and matched_chars >= n * 0.85:
        return parts
    return None

# ─── 全局 RapidOCR 单例（避免批量转换时重复加载 ONNX 模型） ────────
_OCR_ENGINE: Optional[RapidOCR] = None


def get_ocr_engine() -> RapidOCR:
    """获取全局单例 RapidOCR 引擎（针对银行流水密集单元格调优参数）。

    调优要点：
    - rec_batch_num=1 + rec_img_shape=[3, 48, 120]：按单格自适应宽度识别。
    - 保持单线程，避免多页与文字框并行争抢 CPU。
    """
    global _OCR_ENGINE
    if _OCR_ENGINE is None:
        _rapidocr_main.TextClassifier = lambda *a, **kw: None
        _OCR_ENGINE = RapidOCR(
            use_cls=False,               # 银行流水无180度倒置单行，关闭分类器提速
            det_limit_type='max',        # 限制最大边长，避免大图无意义放大拖慢推理
            det_limit_side_len=960,
            det_box_thresh=0.45,
            det_unclip_ratio=1.5,        # 适度收敛检测框，防止密集上下行粘连
            rec_batch_num=1,
            rec_img_shape=[3, 48, 120],
            intra_op_num_threads=1,      # 单会话单线程，不叠加外层线程池
            inter_op_num_threads=1,
        )
    return _OCR_ENGINE


# ═══════════════════════════════════════════════════════
# 图像预处理与几何纠偏工具函数
# ═══════════════════════════════════════════════════════

def remove_red_stamp(img_rgb: np.ndarray) -> np.ndarray:
    """像素级抠除红色印章（含浅粉抗锯齿边缘）并自动修复印章跨越处的黑字笔画断裂。

    处理策略：
    1. 识别所有红章像素 is_red (R-G > 16 且 R-B > 14)；
    2. 对红章掩码区域：
       - 若像素本身偏暗 (R <= 135)，说明底层是深色文字笔画；
       - 或者像素虽为亮红，但其上下或左右紧邻非红色的深色文字笔画（说明红章线条横穿了黑字笔画，
         直接置白会导致字迹断裂），通过邻域连通检测自动桥接还原为深色笔画 (35, 35, 35)！
       - 其余纯白背景上的红章像素一律置为纯白 (255, 255, 255)。
    """
    if img_rgb.ndim != 3 or img_rgb.shape[2] < 3:
        return img_rgb

    out = img_rgb[:, :, :3].copy()
    r = out[:, :, 0].astype(np.int16)
    g = out[:, :, 1].astype(np.int16)
    b = out[:, :, 2].astype(np.int16)

    is_red = ((r - g) > 16) & ((r - b) > 14)
    if not np.any(is_red):
        return out

    # 非红色区域的深色文字掩码
    gray = (r + g + b) // 3
    dark_text = (~is_red) & (gray < 165)

    # 检测红章横穿黑字笔画的桥接点：上下同时邻接深色字迹，或左右同时邻接深色字迹（1~2px 跨度）
    v_bridge = np.zeros_like(is_red)
    h_bridge = np.zeros_like(is_red)
    v_bridge[1:-1, :] = dark_text[:-2, :] & dark_text[2:, :]
    v_bridge[2:-2, :] |= dark_text[:-4, :] & dark_text[4:, :]
    h_bridge[:, 1:-1] = dark_text[:, :-2] & dark_text[:, 2:]
    h_bridge[:, 2:-2] |= dark_text[:, :-4] & dark_text[:, 4:]

    red_on_black = is_red & ((r <= 135) | v_bridge | h_bridge)
    red_on_white = is_red & (~red_on_black)

    out[red_on_white] = (255, 255, 255)
    out[red_on_black] = (35, 35, 35)
    return out


def is_slanted_watermark_box(box: np.ndarray) -> bool:
    """判断检测框是否为斜向防伪水印。

    银行流水的斜向水印通常呈 20°~60° 倾斜（如兴业银行约 -29°），
    而正常扫描件的页面歪斜一般不超过 ±8°。在送入文字识别前直接剔除斜向框，
    既能防止水印混入正文，又能省去大量无效 OCR 识别耗时。
    （注意：不可按 h > w 过滤窄框，因为换行后的单个数字如 '0'/'1'/'8' 本身就是高大于宽！）
    """
    dx = float(box[1][0] - box[0][0])
    dy = float(box[1][1] - box[0][1])
    w = math.hypot(dx, dy)
    h = float(np.linalg.norm(box[0] - box[3]))
    if w <= 1.0 or h <= 1.0:
        return True
    # 宽度大于高度的文本条，若倾角超过 11.5° (tan > 0.203)，必为斜向水印
    if w > h * 1.35 and abs(dy) > abs(dx) * 0.203:
        return True
    return False


# 页面图像级 OCR 结果缓存（当同一进程或批量任务中遇到完全相同的页面位图时直接复用推理结果，节省 CPU）
_PAGE_OCR_CACHE: Dict[str, List[Tuple[list, str, float]]] = {}

# 只复用像素完全一致的文字块，不对金额、日期做近似匹配。
_TEXT_OCR_CACHE: Dict[tuple, tuple] = {}
_TEXT_OCR_CACHE_LIMIT = 4096


def recognize_crops_cached(ocr, crops):
    """逐块精确去重，仍由原识别器单线程执行未命中的文字块。"""
    # 多块合批时，去重会改变填充宽度与批组成，因此保留原始识别路径。
    if ocr.text_rec.rec_batch_num != 1:
        return ocr.text_rec(crops, False)[0]
    keys = [(id(ocr.text_rec), crop.shape, crop.dtype.str,
             hashlib.sha256(crop.tobytes()).digest()) for crop in crops]
    missing = {}
    for key, crop in zip(keys, crops):
        if key not in _TEXT_OCR_CACHE and key not in missing:
            missing[key] = crop
    fresh = {}
    if missing:
        values, _ = ocr.text_rec(list(missing.values()), False)
        fresh = dict(zip(missing, values))
    results = [_TEXT_OCR_CACHE[key] if key in _TEXT_OCR_CACHE else fresh[key] for key in keys]
    for key, value in fresh.items():
        if len(_TEXT_OCR_CACHE) >= _TEXT_OCR_CACHE_LIMIT:
            _TEXT_OCR_CACHE.pop(next(iter(_TEXT_OCR_CACHE)))
        _TEXT_OCR_CACHE[key] = value
    return results


def fast_ocr_infer(img_rgb: np.ndarray) -> List[Tuple[list, str, float]]:
    """执行极速 OCR 推理：先检测并滤除斜向水印框，再串行识别文本。

    Returns:
        [(box_4pts, text, score), ...]
    """
    img_hash = hashlib.md5(img_rgb.tobytes()).hexdigest()
    if img_hash in _PAGE_OCR_CACHE:
        return _PAGE_OCR_CACHE[img_hash]

    ocr = get_ocr_engine()
    raw_h, raw_w = img_rgb.shape[:2]
    op_record = {}
    img, ratio_h, ratio_w = ocr.preprocess(img_rgb)
    op_record["preprocess"] = {"ratio_h": ratio_h, "ratio_w": ratio_w}

    img, op_record = ocr.maybe_add_letterbox(img, op_record)
    dt_boxes, _ = ocr.auto_text_det(img)
    if dt_boxes is None or len(dt_boxes) == 0:
        _PAGE_OCR_CACHE[img_hash] = []
        return []

    # 在进入耗时的识别模型前，先过滤掉斜向水印框
    kept_boxes = [b for b in dt_boxes if not is_slanted_watermark_box(b)]
    if not kept_boxes:
        _PAGE_OCR_CACHE[img_hash] = []
        return []

    crops = ocr.get_crop_img_list(img, kept_boxes)
    orig_boxes = ocr._get_origin_points(kept_boxes, op_record, raw_h, raw_w)

    # 严格串行单线程执行识别（不开启多线程池，避免占用过多 CPU 导致用户电脑卡顿发热）
    rec_res = recognize_crops_cached(ocr, crops)

    results = []
    for box, (text, score) in zip(orig_boxes, rec_res):
        if float(score) < ocr.text_score:
            continue
        clean_t = str(text or '').strip()
        if not clean_t or _WATERMARK_TEXT_RE.match(clean_t):
            continue
        results.append((box.tolist(), clean_t, float(score)))

    if len(_PAGE_OCR_CACHE) < 2500:
        _PAGE_OCR_CACHE[img_hash] = results
    return results


def estimate_skew_angle(ocr_items: list, img_width: float) -> float:
    """从 OCR 文本框四点坐标估算整页水平倾斜角（弧度）。

    同时融合两种几何线索：
    1. 宽文本框自身的上边缘倾角
    2. 同一逻辑行左右跨度较大的文本框中心连线倾角（长基线抗噪）
    """
    angles = []
    min_w = max(img_width * 0.04, 36.0)
    centers = []

    for item in ocr_items:
        box = item[0]
        dx = float(box[1][0] - box[0][0])
        dy = float(box[1][1] - box[0][1])
        h = float(abs(box[3][1] - box[0][1]))
        if dx >= min_w and dx > h * 1.8:
            angle = math.atan2(dy, dx)
            if abs(angle) <= math.radians(8.0):
                angles.append(angle)
        cx = (float(box[0][0]) + float(box[2][0])) * 0.5
        cy = (float(box[0][1]) + float(box[2][1])) * 0.5
        centers.append((cx, cy, max(h, 8.0)))

    # 补充同逻辑行跨列长基线倾角（应对每个单元格独立成短框的表格）
    if len(angles) < 5 and len(centers) >= 4:
        centers.sort(key=lambda c: c[0])
        for i in range(len(centers)):
            x1, y1, h1 = centers[i]
            for j in range(i + 1, len(centers)):
                x2, y2, h2 = centers[j]
                span_x = x2 - x1
                if span_x < img_width * 0.25:
                    continue
                avg_h = (h1 + h2) * 0.5
                if abs(y2 - y1) <= min(avg_h * 0.9, span_x * math.tan(math.radians(5.0))):
                    angles.append(math.atan2(y2 - y1, span_x))

    if len(angles) < 3:
        return 0.0
    angles.sort()
    median_angle = angles[len(angles) // 2]
    if abs(median_angle) < math.radians(0.12):
        return 0.0
    return median_angle


def rotate_point(x: float, y: float, cx: float, cy: float,
                 cos_a: float, sin_a: float) -> Tuple[float, float]:
    """绕中心点 (cx, cy) 旋转坐标点 (x, y)。"""
    rx = (x - cx) * cos_a - (y - cy) * sin_a + cx
    ry = (x - cx) * sin_a + (y - cy) * cos_a + cy
    return rx, ry


def _char_visual_weight(ch: str) -> float:
    """估算单个字符在等比/常规字体下的相对视觉宽度权重。"""
    if ch == ' ':
        return 0.5
    if ch in '.,:;|\'!I1l-':
        return 0.65
    if ord(ch) > 0x2E7F:
        return 1.85
    return 1.0


def split_box_to_chars(text: str, x0: float, x1: float,
                       top: float, bottom: float,
                       allow_atomic: bool = True) -> List[Dict[str, float]]:
    """按字符视觉宽度权重将文本块拆分为单字符坐标字典，并对单列内的连续数字/日期保持同列凝聚。"""
    clean_t = text.strip()
    if not clean_t:
        return []
    weights = [_char_visual_weight(c) for c in clean_t]
    total_w = sum(weights) or 1.0
    span = max(x1 - x0, 1.0)

    char_spans = []
    cur_x = x0
    for ch, w in zip(clean_t, weights):
        cw = span * (w / total_w)
        char_spans.append((ch, cur_x, cur_x + cw))
        cur_x += cw

    if not allow_atomic:
        return [
            {'text': ch, 'x0': cx0, 'x1': cx1, 'top': top, 'bottom': bottom}
            for ch, cx0, cx1 in char_spans
            if ch.strip()
        ]

    chars = []
    for m in _ATOMIC_TOKEN_RE.finditer(clean_t):
        s_idx, e_idx = m.start(), m.end()
        tok = m.group(0)
        if not tok.strip():
            continue
        tx0 = char_spans[s_idx][1]
        tx1 = char_spans[e_idx - 1][2]
        # 仅当单个数字/日期 token 宽度合理 (< 65pt) 时凝聚在中心，避免跨列超长串被聚到一列
        if (len(tok) > 1
                and (tx1 - tx0) < 65.0
                and re.match(r'^[0-9A-Za-z.,:;/+-¥￥$年月日]+$', tok)):
            tcx = (tx0 + tx1) * 0.5
            step = min(0.4, (tx1 - tx0) / max(len(tok), 1))
            start_x = tcx - step * len(tok) * 0.5
            for idx, ch in enumerate(tok):
                cx0 = start_x + idx * step
                chars.append({
                    'text': ch,
                    'x0': cx0,
                    'x1': cx0 + step * 0.9,
                    'top': top,
                    'bottom': bottom,
                })
        else:
            for idx in range(s_idx, e_idx):
                ch, cx0, cx1 = char_spans[idx]
                if ch.strip():
                    chars.append({
                        'text': ch,
                        'x0': cx0,
                        'x1': cx1,
                        'top': top,
                        'bottom': bottom,
                    })
    return chars


def ocr_result_to_words_and_chars(
    ocr_res: Optional[list],
    img_w: float,
    img_h: float,
    scale_to_pt: float = 1.0,
    allow_deskew: bool = True,
    vlines_pt: Optional[List[float]] = None,
) -> Tuple[List[Dict], List[Dict]]:
    """将 OCR 推理结果转换为 pdfplumber 兼容的 words 与 chars 列表。"""
    if not ocr_res:
        return [], []

    skew_rad = estimate_skew_angle(ocr_res, img_w) if allow_deskew else 0.0
    cos_a = math.cos(-skew_rad)
    sin_a = math.sin(-skew_rad)
    cx, cy = img_w / 2.0, img_h / 2.0

    if abs(skew_rad) > 0:
        logger.debug(f"检测到页面倾斜 {math.degrees(skew_rad):.2f}°，已执行坐标级自动纠偏")

    def _transform_box(pts) -> Tuple[float, float, float, float]:
        if abs(skew_rad) > 0:
            rpts = [rotate_point(float(p[0]), float(p[1]), cx, cy, cos_a, sin_a)
                    for p in pts]
        else:
            rpts = [(float(p[0]), float(p[1])) for p in pts]
        xs = [p[0] * scale_to_pt for p in rpts]
        ys = [p[1] * scale_to_pt for p in rpts]
        return min(xs), max(xs), min(ys), max(ys)

    words: List[Dict] = []
    chars: List[Dict] = []

    for item in ocr_res:
        box = item[0]
        raw_text = str(item[1] or '').strip()
        score = float(item[2]) if len(item) > 2 else 1.0
        if not raw_text:
            continue

        # 过滤低置信度的单字符非数字水印残片（保留所有单数字，防止换行秒数被误删）
        if (len(raw_text) == 1
                and score < 0.85
                and not raw_text.isdigit()
                and raw_text not in ('支', '收', '借', '贷')):
            continue

        bx0, bx1, btop, bbottom = _transform_box(box)

        # 检查是否因紧贴表格竖线导致两个单元格内容被合并进同一个检测框
        # 情况 A：「账号/日期 + 中文」（如 '0112070004721138教育培训中心有限' 或 '2026-01-30扫二维码付款'）
        # 情况 B：「中文/英文户名 + 长单号数字」（如微信支付 '涂超群10000500012026013' 或 '刘辉tomorr10000398012026013'）
        # 情况 C：相邻两列金额粘连（如 '750,000.0020,933,362.14'）
        crossing_vlines = [
            vx for vx in (vlines_pt or [])
            if (bx0 + 3.0) < vx < (bx1 - 3.0)
        ]
        m_cross_order_date = (
            re.match(r'^(\d{12,})\s*(20\d{2}[-/.]\d{1,2}[-/.]\d{1,2}.*)$', raw_text)
            if len(crossing_vlines) >= 1 else None
        )
        m_cross_cn = (
            re.match(r'^([+-]?[0-9A-Za-z.,\-:]{4,})\s*([\u4e00-\u9fff（(）)].*)$', raw_text)
            if len(crossing_vlines) == 1 and not m_cross_order_date else None
        )
        m_cross_right_num = (
            re.match(r'^(.+?[^\d\s])\s*(\d{7,}[A-Za-z0-9_\-]*)$', raw_text)
            if len(crossing_vlines) == 1 and not m_cross_cn and not m_cross_order_date else None
        )
        m_cross_amt = (
            re.match(
                r'^([+-]?[¥￥$]?(?:\d{1,3}(?:,\d{3})+|\d+)\.\d{2})\s*([+-]?[¥￥$]?(?:\d{1,3}(?:,\d{3})+|\d+)\.\d{2})$',
                raw_text,
            )
            if len(crossing_vlines) == 1 else None
        )

        box_words: List[Dict] = []
        if m_cross_order_date:
            vx = crossing_vlines[0]
            left_txt, right_txt = m_cross_order_date.group(1).strip(), m_cross_order_date.group(2).strip()
            box_words.append({
                'text': left_txt,
                'x0': bx0 + 1.0,
                'x1': vx - 1.0,
                'top': btop,
                'bottom': bbottom,
            })
            # 若右侧还跨越了第二条竖线且为『日期+中文类型』（如 '2026-01-30扫二维码付款'），继续按第二条竖线切分
            m_sub_cn = re.match(r'^(20\d{2}[-/.]\d{1,2}[-/.]\d{1,2})\s*([\u4e00-\u9fff].*)$', right_txt)
            if m_sub_cn and len(crossing_vlines) >= 2:
                vx2 = crossing_vlines[1]
                box_words.append({
                    'text': m_sub_cn.group(1),
                    'x0': vx + 1.0,
                    'x1': vx2 - 1.0,
                    'top': btop,
                    'bottom': bbottom,
                })
                box_words.append({
                    'text': m_sub_cn.group(2),
                    'x0': vx2 + 1.0,
                    'x1': bx1 - 1.0,
                    'top': btop,
                    'bottom': bbottom,
                })
            else:
                box_words.append({
                    'text': right_txt,
                    'x0': vx + 1.0,
                    'x1': bx1 - 1.0,
                    'top': btop,
                    'bottom': bbottom,
                })
        elif m_cross_cn:
            vx = crossing_vlines[0]
            left_txt, right_txt = m_cross_cn.group(1).strip(), m_cross_cn.group(2).strip()
            box_words.append({
                'text': left_txt,
                'x0': bx0 + 1.0,
                'x1': vx - 1.0,
                'top': btop,
                'bottom': bbottom,
            })
            box_words.append({
                'text': right_txt,
                'x0': vx + 1.0,
                'x1': bx1 - 1.0,
                'top': btop,
                'bottom': bbottom,
            })
        elif m_cross_right_num:
            vx = crossing_vlines[0]
            left_txt, right_txt = m_cross_right_num.group(1).strip(), m_cross_right_num.group(2).strip()
            box_words.append({
                'text': left_txt,
                'x0': bx0 + 1.0,
                'x1': vx - 1.0,
                'top': btop,
                'bottom': bbottom,
            })
            box_words.append({
                'text': right_txt,
                'x0': vx + 1.0,
                'x1': bx1 - 1.0,
                'top': btop,
                'bottom': bbottom,
            })
        elif m_cross_amt:
            vx = crossing_vlines[0]
            left_amt, right_amt = m_cross_amt.group(1).strip(), m_cross_amt.group(2).strip()
            box_words.append({
                'text': left_amt,
                'x0': bx0 + 1.0,
                'x1': vx - 1.0,
                'top': btop,
                'bottom': bbottom,
            })
            box_words.append({
                'text': right_amt,
                'x0': vx + 1.0,
                'x1': bx1 - 1.0,
                'top': btop,
                'bottom': bbottom,
            })
        else:
            hdr_split = split_concat_header_text(raw_text)
            sub_parts = hdr_split if hdr_split else [p for p in re.split(r'\s+', raw_text) if p]
            if len(sub_parts) <= 1:
                box_words.append({
                    'text': raw_text,
                    'x0': bx0,
                    'x1': bx1,
                    'top': btop,
                    'bottom': bbottom,
                })
            else:
                if not hdr_split and re.match(r'^\d{4}[-/.]?\d{1,2}[-/.]?\d{1,2}\s+\d{1,2}:', raw_text):
                    shrink = (bx1 - bx0) * 0.12
                    box_words.append({
                        'text': raw_text,
                        'x0': bx0 + shrink,
                        'x1': bx1 - shrink,
                        'top': btop,
                        'bottom': bbottom,
                    })
                else:
                    span = max(bx1 - bx0, 1.0)
                    weights = [
                        sum(_char_visual_weight(c) for c in p) for p in sub_parts
                    ]
                    gap_ratio = 0.15 if hdr_split else 0.8
                    gap_w = gap_ratio * (len(sub_parts) - 1)
                    total_w = sum(weights) + gap_w
                    cur_x = bx0
                    for p, pw_weight in zip(sub_parts, weights):
                        pw = span * (pw_weight / max(total_w, 1.0))
                        box_words.append({
                            'text': p,
                            'x0': cur_x,
                            'x1': cur_x + pw,
                            'top': btop,
                            'bottom': bbottom,
                        })
                        cur_x += pw + span * (gap_ratio / max(total_w, 1.0))

        words.extend(box_words)
        for bw in box_words:
            w_span = bw['x1'] - bw['x0']
            # 若该子词内部跨越了表格竖线，绝不可向中心收缩 pad 或执行跨竖线的原子凝聚（否则右侧单元格的首位数字会被拉过竖线掉进左列！）
            crosses_v = any((bw['x0'] + 4.0) < vx < (bw['x1'] - 4.0) for vx in (vlines_pt or []))
            pad = 0.0 if crosses_v else w_span * 0.08
            chars.extend(
                split_box_to_chars(
                    bw['text'],
                    bw['x0'] + pad,
                    bw['x1'] - pad,
                    bw['top'],
                    bw['bottom'],
                    allow_atomic=(not crosses_v),
                )
            )

    return words, chars


# 常见 OCR 多行窄列表头形近错字规范化映射（如建行17列紧凑表头）
_HEADER_NAME_FIXES = {
    '借方发生额取)': '借方发生额(支取)',
    '借方发生额(支取': '借方发生额(支取)',
    '借方发生额支取)': '借方发生额(支取)',
    '借方发生额支取': '借方发生额(支取)',
    '贷方发生额收': '贷方发生额(收入)',
    '贷方发生额(收': '贷方发生额(收入)',
    '贷方发生额收入)': '贷方发生额(收入)',
    '贷方发生额收入': '贷方发生额(收入)',
    '账户明细编号交易流永号': '账户明细编号-交易流水号',
    '账户明细编号-交易流永号': '账户明细编号-交易流水号',
    '账户明细编号交易流水号': '账户明细编号-交易流水号',
    '细编号-交易流永号': '账户明细编号-交易流水号',
    '细编号-交易流水号': '账户明细编号-交易流水号',
    '细编号交易流永号': '账户明细编号-交易流水号',
    '企业流永号': '企业流水号',
}


def normalize_ocr_header_name(name: str) -> str:
    """对 OCR 识别出的表头列名进行形近错字修复与规范化。"""
    s = sanitize(_clean_col_name(name)).replace(' ', '')
    if s in _HEADER_NAME_FIXES:
        return _HEADER_NAME_FIXES[s]
    s = s.replace('流永号', '流水号')
    if s.startswith('借方发生额') and ('支' in s or '取' in s or ')' in s):
        return '借方发生额(支取)'
    if s.startswith('贷方发生额') and ('收' in s or '入' in s or ')' in s):
        return '贷方发生额(收入)'
    if '明细编号' in s and '流水号' in s:
        return '账户明细编号-交易流水号'
    return s


def ocr_chars_to_row(chars: list, col_bounds: list) -> List[str]:
    """OCR 专用字符分桶拼行函数：

    与原版 chars_to_row 的区别：单元格内跨行换行时，默认直接无缝拼接（不插入空格！），
    彻底消除长账号、长流水号、长户名、跨行金额因单元格换行产生的怪异空格（如 '1610- 43066258'、'509109.6 0'）；
    仅在上一行结尾是完整日期（如 '2025-10-01' 或 '20250701'）且下一行开头是时间（如 '04:38:41'）时保留一个空格。
    """
    buckets = [[] for _ in col_bounds]
    for c in chars:
        if not c['text'].strip():
            continue
        cx = (c['x0'] + c['x1']) / 2
        assigned = -1
        for i, (x0, x1) in enumerate(col_bounds):
            if x0 - 2 <= cx <= x1 + 2:
                assigned = i
                break
        if assigned < 0:
            best_d, best_i = 999, 0
            for i, (x0, x1) in enumerate(col_bounds):
                d = min(abs(cx - x0), abs(cx - x1))
                if d < best_d:
                    best_d, best_i = d, i
            if best_d <= 15:
                assigned = best_i
        if assigned >= 0:
            buckets[assigned].append(c)

    row = []
    for b in buckets:
        if not b:
            row.append('')
        else:
            b.sort(key=lambda c: (round(c['top'] / 4) * 4, c['x0']))
            lines = []
            cur_line = [b[0]['text']]
            prev_top = round(b[0]['top'] / 4) * 4
            for c in b[1:]:
                cur_top = round(c['top'] / 4) * 4
                if abs(cur_top - prev_top) > 3:
                    lines.append(''.join(cur_line).strip())
                    cur_line = [c['text']]
                else:
                    cur_line.append(c['text'])
                prev_top = cur_top
            lines.append(''.join(cur_line).strip())
            lines = [ln for ln in lines if ln]

            if not lines:
                row.append('')
            elif len(lines) == 1:
                row.append(sanitize(lines[0]))
            else:
                out_s = lines[0]
                for nxt in lines[1:]:
                    # 仅当上一行是日期（如 2025-10-01 或 20250701）、下一行是时间（04:37:34）时加空格
                    if (re.search(r'(?:\d{4}[-/.]\d{1,2}[-/.]\d{1,2}|\b20\d{6})$', out_s)
                            and re.match(r'^\d{1,2}:\d{2}', nxt)):
                        out_s = f"{out_s} {nxt}"
                    else:
                        out_s = f"{out_s}{nxt}"
                row.append(sanitize(out_s))
    return row
# 页面底部表外长篇说明段截断正则（仅匹配明确的页尾证明说明条款与免责声明行首，绝不可包含『财付通』等可能出现在交易对手列中的词，也不可包含兴业银行页中穿插的『交易明细涉及您的』！）
_PAGE_FOOTER_CUTOFF_RE = re.compile(
    r'(?:^说明[:：]$|^说明[:：]\s*1[.、]|^[1-5][.、]\s*本?《微信支付|'
    r'仅证明[:：]在用户选择|仅用于用户申请本证明|不得用于其他用途|账户记录为准|'
    r'对于红包[，,]转账[，,]二维码|零钱的充值和提现|'
    r'^重要提示[:：]|^温馨提示[:：]|^该交易明细因不可预测|'
    r'^(?:打印完毕|打印完成|打印结束)[。.!！]?$)'
)


def truncate_page_footer_chars(chars: list, min_y: float = 0.0) -> list:
    """通用表尾截断器：按水平行自上而下扫描，一旦在数据区发现表外说明、汇总、提示或落款行，
    立即将该行高度作为本页表格底界，丢弃该行及其下方的全部字符，彻底防止表尾文字被塞入最后一行！
    """
    if not chars:
        return chars
    data_chars = [c for c in chars if c['top'] > min_y]
    if not data_chars:
        return chars
    lines = group_by_y(data_chars, tol=5.0)
    cutoff_y = None
    for line_index, rg in enumerate(lines):
        line_str = ''.join(c['text'] for c in sorted(rg, key=lambda c: c['x0'])).replace(' ', '')
        if re.fullmatch(r'(?:打印完毕|打印完成|打印结束)[。.!！]?', line_str):
            # 若后面仍有日期开头的交易行，此处可能是摘要续行，不能提前截断。
            if any(starts_with_date(''.join(c['text'] for c in sorted(row, key=lambda c: c['x0'])))
                   for row in lines[line_index + 1:]):
                continue
        if _PAGE_FOOTER_CUTOFF_RE.search(line_str):
            cutoff_y = min(c['top'] for c in rg) - 1.5
            break
    if cutoff_y is not None:
        return [c for c in chars if c['top'] < cutoff_y]
    return chars
def _is_english_header_line(text: str) -> bool:
    """判断一行文本是否为中英双语表头的纯英文翻译行（允许含 / . ' - 等标点）。"""
    plain = text.replace(' ', '').strip()
    if not plain or not _ENGLISH_HDR_LINE_RE.match(plain):
        return False
    # 至少包含 3 个英文字母，避免把单个符号误判为英文表头
    return sum(1 for c in plain if c.isalpha()) >= 3


# ═══════════════════════════════════════════════════════
# 虚拟页面对象（无缝桥接 BankPDFParser 的算法）
# ═══════════════════════════════════════════════════════

def detect_image_table_lines(
    img_rgb: np.ndarray, scale_to_pt: float
) -> Tuple[List[float], List[float]]:
    """从扫描件/渲染图像中极速检测表格横线 (hlines) 与竖线 (vlines) 坐标（单位：pt）。

    注意：绝不擦除图像上的表格线像素（擦除会削掉紧贴边框的文字笔画）；
    跨竖线粘连文本在 ocr_result_to_words_and_chars 中通过 vlines_pt 坐标无损切分。
    """
    h, w = img_rgb.shape[:2]
    if h < 100 or w < 100:
        return [], []

    gray = img_rgb[:, :, :3].mean(axis=2)
    dark_mask = gray < 236

    # 1. 检测横线：宽度超过页宽 42% 的细水平线，且上下两侧留有空白（排除微信支付等高密度文字行被误判为横线！）
    row_ratio = dark_mask.mean(axis=1)
    line_ys = np.where(row_ratio > 0.42)[0]
    merged_ys = []
    for y in line_ys:
        if not merged_ys or int(y) - merged_ys[-1][-1] > 4:
            merged_ys.append([int(y)])
        else:
            merged_ys[-1].append(int(y))

    valid_h_groups = []
    for g in merged_ys:
        if len(g) > 6:
            continue
        y_up = max(0, g[0] - 3)
        y_dn = min(h - 1, g[-1] + 3)
        # 真正的表格横线上下 3px 处必然是单元格内留白（暗像素占比低），而密集文字行内部上下均高
        if row_ratio[y_up] < 0.22 and row_ratio[y_dn] < 0.22:
            valid_h_groups.append(g)

    hlines_px = [float(np.mean(g)) for g in valid_h_groups]
    hlines_pt = [round(y * scale_to_pt, 1) for y in hlines_px]

    # 2. 若存在有效横线，先定位密集的表格主横线簇（排除页面顶部独立证明框或底部远离表格的页脚线），
    #    再在表格主体区间内检测连续实心竖线 (col_ratio > 0.86)
    vlines_pt = []
    if len(hlines_px) >= 4:
        chains = [[hlines_px[0]]]
        for y in hlines_px[1:]:
            if (y - chains[-1][-1]) * scale_to_pt <= 65.0:
                chains[-1].append(y)
            else:
                chains.append([y])
        best_chain = max(chains, key=len)
        if len(best_chain) >= 3:
            best_v_px = []
            max_k_start = min(5, len(best_chain) - 2)
            cand_ends = [len(best_chain) - 1]
            if len(best_chain) >= 6:
                cand_ends.append(len(best_chain) - 2)
            for k_start in range(max_k_start):
                for k_end in cand_ends:
                    if k_end <= k_start + 1:
                        continue
                    y_top = int(best_chain[k_start])
                    y_bot = int(best_chain[k_end])
                    if y_bot - y_top <= 60:
                        continue
                    sub = dark_mask[y_top:y_bot, :]
                    col_ratio = sub.mean(axis=0)
                    line_xs = np.where(col_ratio > 0.86)[0]
                    merged_xs = []
                    for x in line_xs:
                        if not merged_xs or int(x) - merged_xs[-1][-1] > 4:
                            merged_xs.append([int(x)])
                        else:
                            merged_xs[-1].append(int(x))
                    cand_v_px = [float(np.mean(g)) for g in merged_xs if len(g) <= 7]
                    if len(cand_v_px) > len(best_v_px):
                        best_v_px = cand_v_px
            if len(best_v_px) >= 4:
                vlines_pt = [round(x * scale_to_pt, 1) for x in best_v_px]

    return hlines_pt, vlines_pt


class VirtualOCRPage:
    """模拟 pdfplumber.page.Page 的轻量接口，使 BankPDFParser 算法可直接复用。"""

    def __init__(self, width: float, height: float,
                 words: List[Dict], chars: List[Dict],
                 hlines_pt: Optional[List[float]] = None,
                 vlines_pt: Optional[List[float]] = None):
        self.width = width
        self.height = height
        self._words = words
        self.chars = chars
        self.hlines_pt = hlines_pt or []
        self.vlines_pt = vlines_pt or []
        self.lines = [
            {'top': y, 'bottom': y, 'height': 1.0, 'width': width * 0.8}
            for y in self.hlines_pt
        ]

    def extract_words(self, **kwargs) -> List[Dict]:
        return self._words


# ═══════════════════════════════════════════════════════
# 金融数字纠偏与借贷余额钩稽校验
# ═══════════════════════════════════════════════════════

_AMOUNT_KW = ('金额', '借方', '贷方', '收入', '支出', '出账', '入账', '发生额')
_BALANCE_KW = ('余额', '本次余额', '账户余额')
_DEBIT_KW = ('借方', '支出', '出账', '付方')
_CREDIT_KW = ('贷方', '收入', '入账', '收方')

_OCR_DIGIT_TRANS = str.maketrans({
    'O': '0', 'o': '0', 'D': '0', 'Q': '0', 'J': '0',
    'l': '1', 'I': '1', '|': '1',
    'Z': '2', 'z': '2',
    'S': '5', 's': '5',
    'b': '6', 'G': '6',
    'B': '8', '$': '8',
    'g': '9', 'q': '9',
    '，': ',', '。': '.',
})


def clean_ocr_amount(val: str) -> str:
    """清洗并修复 OCR 识别的金额/余额文本。"""
    if not val or not val.strip():
        return val
    s = val.strip()
    if any(ord(c) > 0x2E7F for c in s):
        return s

    prefix = ''
    m_pre = re.match(r'^([¥￥$+-]+)\s*(.*)$', s)
    if m_pre:
        prefix, body = m_pre.group(1), m_pre.group(2)
    else:
        body = s

    cleaned = body.translate(_OCR_DIGIT_TRANS).replace(' ', '')
    if not re.match(r'^[\d,.]+$', cleaned):
        return s

    if cleaned.count('.') > 1:
        parts = cleaned.rsplit('.', 1)
        cleaned = parts[0].replace('.', ',') + '.' + parts[1]
    elif '.' not in cleaned and ',' in cleaned:
        # 情况 A：'38,86614'（有千分位逗号，但末尾两位小数前漏了小数点）
        if re.match(r'^\d{1,3}(?:,\d{3})+\d{2}$', cleaned):
            cleaned = cleaned[:-2] + '.' + cleaned[-2:]
        else:
            # 情况 B：'1234,56'（末尾小数点被误识为逗号）
            parts = cleaned.rsplit(',', 1)
            if len(parts[1]) == 2 and len(parts[0]) >= 1:
                cleaned = parts[0] + '.' + parts[1]

    return prefix + cleaned


def _parse_float(val: str) -> Optional[float]:
    """将金额字符串解析为浮点数，无法解析时返回 None。"""
    if not val or not val.strip():
        return None
    s = val.strip().replace(',', '').replace('¥', '').replace('￥', '').replace(' ', '')
    try:
        return float(s)
    except ValueError:
        return None


def _format_like(num: float, ref_str: str) -> str:
    """按参考字符串的千分位风格格式化修复后的金额。"""
    use_comma = ',' in ref_str or abs(num) >= 1000
    if use_comma:
        return f"{num:,.2f}"
    return f"{num:.2f}"


# ═══════════════════════════════════════════════════════
# 核心 OCR 解析引擎（继承复用 BankPDFParser 后处理能力）
# ═══════════════════════════════════════════════════════

class BankOCRParser(pdf_to_excel.BankPDFParser):
    """扫描件 PDF 与流水图片专用 OCR 解析器。

    与 BankPDFParser 保持完全相同的属性结构 (.metadata, .col_names, .data_rows)，
    可直接交由 pdf_to_excel.write_excel() 生成格式一致的 Excel 文件。
    """

    def __init__(self, path: str,
                 password: Optional[str] = None,
                 progress_callback: Optional[Callable[[int, int], None]] = None,
                 cancel_check: Optional[Callable[[], bool]] = None,
                 dpi: int = 144):
        super().__init__(
            path=path,
            password=password,
            progress_callback=progress_callback,
            cancel_check=cancel_check,
        )
        self._table_fmt: Optional[str] = None
        self.dpi = dpi

    def parse(self):
        p = Path(self.path)
        ext = p.suffix.lower()

        if ext in IMAGE_EXTENSIONS:
            self._parse_image_file(p)
        else:
            self._parse_pdf_file(p)

        # 统一过滤可能残留在页尾的汇总块、重要提示块与纯英文表头翻译行
        self.data_rows = [
            r for r in self.data_rows
            if not self._is_metadata_zone_row(r) and not self._is_english_row(r)
        ]

        # 复用 BankPDFParser 经过验证的跨行合并、跨页合并与日期后处理
        self.data_rows = self._merge_cont(self.data_rows)
        self.data_rows = self._merge_cross_page(self.data_rows)
        self._postprocess_dates()
        self._postprocess_ocr_dates()
        self._postprocess_incomplete_records()

        # 强制剔除跨页续连时混入的非交易明细行（有效流水记录在合并后必须在日期列包含合法日期）
        self._filter_non_transaction_rows()

        # 清洗首页元数据中多余的纯英文翻译行（如 Account Name: / Currency: 等）
        self._clean_english_metadata()

        # OCR 专属金融后处理：金额形近字修复 + 借贷余额公式核验 + 同对手方账号/户名上下文一致性归位
        self._postprocess_ocr_numbers()
        self._verify_and_repair_balances()
        self._unify_counterparty_accounts_and_names()

        logger.info(
            f"OCR 解析完成: {len(self.data_rows)} 行数据, {len(self.col_names)} 列")
        return self

    def _parse_image_file(self, p: Path) -> None:
        """解析单张流水图片（手机拍照/扫描图片/长截图），坐标归一化至标准 A4 宽度 (595pt)。"""
        logger.info(f"OCR 解析图片: {p.name}")
        with Image.open(p) as pil_img:
            pil_img = ImageOps.exif_transpose(pil_img).convert('RGB')
            img_rgb = np.array(pil_img)

        h, w = img_rgb.shape[:2]
        scale_to_pt = 595.0 / max(float(w), 1.0)
        vpage = self._ocr_image_to_virtual_page(img_rgb, scale_to_pt=scale_to_pt)
        self._process_virtual_page(vpage, is_first_page=True)
        if self._progress_callback:
            self._progress_callback(1, 1)

    def _parse_pdf_file(self, p: Path) -> None:
        """使用 pypdfium2 渲染 PDF 页面并逐页执行 OCR 解析。"""
        scale = self.dpi / 72.0
        scale_to_pt = 72.0 / self.dpi

        try:
            doc = pdfium.PdfDocument(str(p), password=self.password or None)
        except Exception as e:
            if 'password' in str(e).lower():
                from pdfminer.pdfdocument import PDFPasswordIncorrect
                raise PDFPasswordIncorrect(str(e)) from e
            raise

        try:
            total_pages = len(doc)
            logger.info(
                f"OCR 解析 PDF: {p.name} | 页数: {total_pages} | 渲染分辨率: {self.dpi} DPI")

            # 逐页顺序处理（避免多页并发 + 框并发叠加占满 CPU 造成电脑卡顿）
            for i in range(total_pages):
                if self._cancel_check and self._cancel_check():
                    logger.info("用户取消 OCR 解析")
                    break

                pdf_page = doc[i]
                self._strip_pdf_rotated_watermarks(pdf_page)
                img_rgb = np.array(
                    pdf_page.render(scale=scale, draw_annots=False).to_pil().convert('RGB'))
                vpage = self._ocr_image_to_virtual_page(
                    img_rgb, scale_to_pt=scale_to_pt)
                self._process_virtual_page(vpage, is_first_page=(i == 0))

                if self._progress_callback:
                    self._progress_callback(i + 1, total_pages)
        finally:
            doc.close()

    @staticmethod
    def _strip_pdf_rotated_watermarks(pdf_page) -> None:
        """在 pypdfium2 渲染页面前，直接从 PDF 对象层与批注层抠除：
        1. 斜向（8°~82°）防伪水印文本对象（保留 0° 与 90° 正交排版正文）；
        2. 局部叠加的电子印章/红章图片对象（宽高 < 页面尺寸 45% 的独立小图，如 90x90pt 的电子回单章）；
        3. 页面上的电子签章批注对象（PDF Stamp/Widget Annotations，常见于流水末页），
           从源头彻底消除印章遮挡造成的底层文字笔画断裂！
        """
        try:
            import pypdfium2.raw as pdfium_c
            removed = 0
            pw, ph = pdf_page.get_size()
            for obj in list(pdf_page.get_objects()):
                if obj.type == pdfium_c.FPDF_PAGEOBJ_TEXT:
                    m = obj.get_matrix()
                    # 斜向旋转矩阵特征：水平分量 (a,d) 与交叉分量 (b,c) 同时显著非零
                    if (abs(m.a) > 0.08 and abs(m.b) > 0.08
                            and abs(m.c) > 0.08 and abs(m.d) > 0.08):
                        pdf_page.remove_obj(obj)
                        removed += 1
                elif obj.type == pdfium_c.FPDF_PAGEOBJ_IMAGE:
                    x0, y0, x1, y1 = obj.get_bounds()
                    if (x1 - x0) < pw * 0.45 and (y1 - y0) < ph * 0.45:
                        pdf_page.remove_obj(obj)
                        removed += 1
            annot_cnt = pdfium_c.FPDFPage_GetAnnotCount(pdf_page)
            for a_idx in range(annot_cnt - 1, -1, -1):
                pdfium_c.FPDFPage_RemoveAnnot(pdf_page, a_idx)
                removed += 1
            if removed > 0:
                pdf_page.gen_content()
        except Exception:
            pass

    @staticmethod
    def _strip_pdf_thin_grid_paths(pdf_page) -> bool:
        """若页面含原生文本对象，剥离细长表格线路径对象 (FPDF_PAGEOBJ_PATH)，防止表格线压住文字笔画影响 OCR。"""
        try:
            import pypdfium2.raw as pdfium_c
            objs = list(pdf_page.get_objects())
            has_text = any(o.type == pdfium_c.FPDF_PAGEOBJ_TEXT for o in objs)
            if not has_text:
                return False
            removed = 0
            for obj in objs:
                if obj.type == pdfium_c.FPDF_PAGEOBJ_PATH:
                    x0, y0, x1, y1 = obj.get_bounds()
                    if (x1 - x0) < 3.0 or (y1 - y0) < 3.0:
                        pdf_page.remove_obj(obj)
                        removed += 1
            if removed > 0:
                pdf_page.gen_content()
                return True
        except Exception:
            pass
        return False

    def _ocr_image_to_virtual_page(
            self, img_rgb: np.ndarray, scale_to_pt: float,
            pre_hlines_pt: Optional[List[float]] = None,
            pre_vlines_pt: Optional[List[float]] = None) -> VirtualOCRPage:
        """对 RGB 图像执行红章剥离、表格线检测、极速 OCR 推理，并构建 VirtualOCRPage。"""
        h, w = img_rgb.shape[:2]
        cleaned_img = remove_red_stamp(img_rgb)
        if pre_hlines_pt is not None and pre_vlines_pt is not None:
            hlines_pt, vlines_pt = pre_hlines_pt, pre_vlines_pt
        else:
            hlines_pt, vlines_pt = detect_image_table_lines(cleaned_img, scale_to_pt)
        # 若后续页因行数较少未检出竖线，复用首页已确认的列边界作为竖线参考，确保跨列粘连框仍能精准切开！
        effective_vlines = vlines_pt
        if len(effective_vlines) < 4 and self.col_bounds:
            effective_vlines = [self.col_bounds[0][0]] + [b[1] for b in self.col_bounds]
        ocr_res = fast_ocr_infer(cleaned_img)

        words, chars = ocr_result_to_words_and_chars(
            ocr_res,
            img_w=float(w),
            img_h=float(h),
            scale_to_pt=scale_to_pt,
            allow_deskew=(len(hlines_pt) < 4),
            vlines_pt=effective_vlines,
        )

        return VirtualOCRPage(
            width=float(w) * scale_to_pt,
            height=float(h) * scale_to_pt,
            words=words,
            chars=chars,
            hlines_pt=hlines_pt,
            vlines_pt=vlines_pt,
        )

    def _find_header(self, row_groups: list,
                     hlines_pt: Optional[List[float]] = None) -> Tuple[int, int, float]:
        """OCR 增强版表头检测：

        1. 逐词过滤含连续数字或货币符号的干扰词（既排除汇总行 '支出￥530069.85'，
           又避免微信支付等首行单号因轻微倾斜与表头同行时把整行表头判为0分）。
        2. 若存在物理表格横线 hlines_pt，禁止表头跨横线向下吞并首条数据行的换行文字（如建行）。
        3. 增强中英双语表头的英文翻译行识别（支持含 '/'、'.'、''' 的多行英文列名）。
        """
        scores = []
        for g in row_groups:
            hdr_words = [
                w for w in g
                if not re.search(r'\d{2,}|[￥¥$：:]', w['text'])
            ]
            line_text = ' '.join(w['text'] for w in hdr_words)
            score = sum(1 for k in UNIVERSAL_HEADER_KW if k in line_text)
            scores.append(score)

        best = -1
        max_s = 0
        for i, s in enumerate(scores):
            if s > max_s:
                max_s, best = s, i
        if best < 0 or max_s < 3:
            # 兜底：检查相邻两行合计得分是否 >= 4（应对多行表头单行得分仅 2 分的情况）
            for i in range(len(scores) - 1):
                if scores[i] >= 1 and scores[i + 1] >= 1 and (scores[i] + scores[i + 1]) >= 4:
                    if (row_groups[i + 1][0]['top'] - row_groups[i][0]['top']) < 22:
                        best = i
                        max_s = scores[i] + scores[i + 1]
                        break
        if best < 0 or max_s < 3:
            return -1, -1, 0.0

        # 剔除确认表头行中混入的长数字（如首行数据单号）
        row_groups[best] = [
            w for w in row_groups[best]
            if not re.search(r'\d{4,}', w['text'])
        ]
        confirmed_x0s = {round(w['x0'], 0) for w in row_groups[best]}

        def _crosses_hline(grp_a: list, grp_b: list) -> bool:
            if not hlines_pt:
                return False
            yc_a = sum((w['top'] + w['bottom']) * 0.5 for w in grp_a) / max(len(grp_a), 1)
            yc_b = sum((w['top'] + w['bottom']) * 0.5 for w in grp_b) / max(len(grp_b), 1)
            low, high = min(yc_a, yc_b), max(yc_a, yc_b)
            return any(low < hl < high for hl in hlines_pt)

        def _is_ocr_header_cont(candidate_group) -> bool:
            raw_cand_text = ' '.join(w['text'] for w in candidate_group)
            if '：' in raw_cand_text or ':' in raw_cand_text:
                return False
            # 只要原始行包含数字或货币符号，必为汇总行或数据行（如建行首条数据行），绝不可并入表头！
            if re.search(r'[0-9￥¥$]', raw_cand_text):
                return False
            if _is_english_header_line(raw_cand_text):
                return False
            cand_score = sum(
                1 for k in UNIVERSAL_HEADER_KW if k in raw_cand_text)
            if cand_score >= 1:
                return True
            if len(candidate_group) > 5:
                return False
            plain = raw_cand_text.replace(' ', '')
            if re.search(r'[0-9a-zA-Z]', plain):
                return False
            aligned = sum(
                1 for w in candidate_group
                if any(abs(round(w['x0'], 0) - cx0) < 15 for cx0 in confirmed_x0s)
            )
            return aligned == len(candidate_group)

        start, end = best, best
        while (start > 0
               and (row_groups[start][0]['top']
                    - row_groups[start - 1][0]['top']) < 20):
            if _crosses_hline(row_groups[start - 1], row_groups[start]):
                break
            if _is_ocr_header_cont(row_groups[start - 1]):
                for w in row_groups[start - 1]:
                    confirmed_x0s.add(round(w['x0'], 0))
                start -= 1
            else:
                break

        while (end < len(row_groups) - 1
               and (row_groups[end + 1][0]['top']
                    - row_groups[end][0]['top']) < 20):
            if _crosses_hline(row_groups[end], row_groups[end + 1]):
                break
            if _is_ocr_header_cont(row_groups[end + 1]):
                for w in row_groups[end + 1]:
                    confirmed_x0s.add(round(w['x0'], 0))
                end += 1
            else:
                break

        # 向下扫描紧跟表头的多行英文翻译行（如 Transaction Time / Counterparty's Account Bank）
        extra_bottom = 0.0
        scan = end + 1
        while (scan < len(row_groups)
               and (row_groups[scan][0]['top']
                    - row_groups[max(scan - 1, end)][0]['top']) < 25):
            candidate_text = ' '.join(w['text'] for w in row_groups[scan])
            if _is_english_header_line(candidate_text):
                extra_bottom = max(w['bottom'] for w in row_groups[scan])
                scan += 1
                continue
            break

        return start, end, extra_bottom

    def _detect_page_header_bottom(self, page) -> float:
        """检测后续页重复表头的底部 Y 坐标（含带标点的多行英文表头翻译行）。"""
        if not self.col_names:
            return 0.0
        wds = page.extract_words()
        if not wds:
            return 0.0

        rows = group_by_y(wds, tol=6.5)
        for i, row in enumerate(rows):
            line_text = ' '.join(w['text'] for w in row)
            match_count = sum(
                1 for cn in self.col_names if cn and cn in line_text)
            if match_count >= 3:
                bottom = max(w['bottom'] for w in row)
                scan = i + 1
                while scan < len(rows):
                    next_text = ' '.join(w['text'] for w in rows[scan])
                    if _is_english_header_line(next_text):
                        bottom = max(bottom, max(w['bottom'] for w in rows[scan]))
                        scan += 1
                    else:
                        break
                return bottom
        return 0.0

    def _is_header_row(self, row: list) -> bool:
        """判断一行数据是否是重复的表头行（允许个别列存在轻微 OCR 差异）。"""
        if not self.col_names or not row:
            return False
        n = min(len(row), len(self.col_names))
        if n == 0:
            return False
        matches = sum(
            1 for i in range(n)
            if self.col_names[i] and self.col_names[i] in sanitize(row[i])
        )
        return matches >= max(3, n // 2)

    def _row_start_x(self, chars: list) -> Optional[float]:
        """OCR 增强版页面主数据行起始 X 估算：

        使用 6pt 容差聚合逻辑行（避免 OCR 同行不同框因 1~3pt 高度差被拆散），
        且仅统计从页面左侧 25% 区域内起排的主数据行（防止某行左侧为空、仅右侧『对方户名』
        字数较多时将整页列边界错误右移数十乃至上百 pt！）。
        """
        if not chars:
            return None
        logical_rows = group_by_y(chars, tol=6.0)
        if not logical_rows:
            return None
        max_len = max(len(r) for r in logical_rows)
        if max_len < 8:
            return None
        starts = sorted(
            min(c['x0'] for c in r)
            for r in logical_rows
            if len(r) >= max_len * 0.45 and min(c['x0'] for c in r) < 160.0
        )
        return starts[len(starts) // 2] if starts else None

    def _page_col_bounds(self, all_chars: list,
                         is_first_page: bool) -> list:
        """计算本页专用的列边界，校正整页微小水平偏移（限制最大合理偏移 <= 18pt）。"""
        cur_x = self._row_start_x(all_chars)
        if is_first_page:
            self._base_row_x = cur_x
            return self.col_bounds
        if cur_x is None or self._base_row_x is None:
            return self.col_bounds
        shift = cur_x - self._base_row_x
        if abs(shift) < 2.0 or abs(shift) > 18.0:
            return self.col_bounds
        return [(x0 + shift, x1 + shift) for x0, x1 in self.col_bounds]

    def _process_virtual_page(self, vpage: VirtualOCRPage,
                              is_first_page: bool) -> None:
        """在虚拟页面上执行表头识别、元数据提取与记录分列。"""
        if is_first_page:
            has_zones = len(vpage.hlines_pt) >= 4
            self._table_fmt = 'zones' if has_zones else 'text'
        fmt = self._table_fmt or ('zones' if len(vpage.hlines_pt) >= 4 else 'text')

        if is_first_page:
            # 若页面同时具备清晰的物理横线与竖线网格（如建行17列、工行13列、兴业9列、湖南银行8列等），
            # 优先直接利用物理网格单元 [hlines[k], hlines[k+1]] x [vlines[i], vlines[i+1]] 提取表头！
            grid_hdr_ok = False
            if len(vpage.vlines_pt) >= 5 and len(vpage.hlines_pt) >= 2:
                v_bounds = [
                    (vpage.vlines_pt[i], vpage.vlines_pt[i + 1])
                    for i in range(len(vpage.vlines_pt) - 1)
                ]
                for hk in range(min(6, len(vpage.hlines_pt) - 1)):
                    hy0, hy1 = vpage.hlines_pt[hk], vpage.hlines_pt[hk + 1]
                    if hy1 - hy0 < 8.0 or hy1 - hy0 > 80.0:
                        continue
                    h_chars = [
                        c for c in vpage.chars
                        if hy0 < (c['top'] + c['bottom']) * 0.5 <= hy1
                    ]
                    if not h_chars:
                        continue
                    cand_names = [
                        normalize_ocr_header_name(cell)
                        for cell in ocr_chars_to_row(h_chars, v_bounds)
                    ]
                    kw_hits = sum(
                        1 for cn in cand_names
                        if cn and any(k in cn for k in UNIVERSAL_HEADER_KW)
                    )
                    if kw_hits >= 3 and sum(1 for cn in cand_names if cn) >= len(v_bounds) * 0.6:
                        # 若表头格内既有中文又有紧跟的英文（如兴业 '交易时间TransactionTime'），剥离尾部纯英文
                        cleaned_cand_names = []
                        for cn in cand_names:
                            if any(ord(ch) > 0x2E7F for ch in cn):
                                cn = re.sub(r'[A-Za-z/.\'’()\-]+$', '', cn).strip() or cn
                            cleaned_cand_names.append(normalize_ocr_header_name(cn))
                        if (len(cleaned_cand_names) == 8
                                and any('交易单号' in n for n in cleaned_cand_names)
                                and any('商户单号' in n for n in cleaned_cand_names)):
                            cleaned_cand_names = [
                                '交易单号', '交易时间', '交易类型', '收/支/其他',
                                '交易方式', '金额(元)', '交易对方', '商户单号',
                            ]
                        self.col_names = cleaned_cand_names
                        self.col_bounds = v_bounds
                        self.hdr_bottom = hy1
                        self.metadata = self._extract_meta(vpage, hy0 + 2.0)
                        grid_hdr_ok = True
                        logger.info(
                            f"OCR 网格精确检测到 {len(self.col_names)} 列 ({fmt}模式): {self.col_names}")
                        break

            if not grid_hdr_ok:
                all_rows = group_by_y(vpage.extract_words(), tol=6.5)
                hs, he, extra_bottom = self._find_header(all_rows, vpage.hlines_pt)

                if hs >= 0:
                    hdr_groups = all_rows[hs: he + 1]
                    hdr_top = min(w['top'] for g in hdr_groups for w in g)
                    if vpage.hlines_pt and vpage.hlines_pt[0] < hdr_top:
                        meta_limit = vpage.hlines_pt[0] + 2.0
                    else:
                        meta_limit = hdr_top - 1.0
                    self.hdr_bottom = max(w['bottom'] for g in hdr_groups for w in g)
                    if extra_bottom > self.hdr_bottom:
                        self.hdr_bottom = extra_bottom
                    self.col_names, self.col_bounds = self._build_ocr_cols(
                        hdr_groups, vpage.width, vpage.vlines_pt)
                    self.metadata = self._extract_meta(vpage, meta_limit)
                    logger.info(
                        f"OCR 检测到 {len(self.col_names)} 列 ({fmt}模式): {self.col_names}")
                else:
                    self.metadata = self._extract_meta(vpage, vpage.height)
                    logger.warning("OCR 未能自动检测到标准表头，尝试启发式分列")
                    self._fallback_columns_if_needed(vpage)

            rows = self._extract_ocr_page_rows(vpage, fmt, is_first_page=True)
            self.data_rows.extend(rows)
        else:
            for r in self._extract_ocr_page_rows(vpage, fmt, is_first_page=False):
                if not self._is_header_row(r):
                    self.data_rows.append(r)

    def _extract_ocr_page_rows(self, vpage: VirtualOCRPage,
                               fmt: str, is_first_page: bool) -> list:
        """提取单页 OCR 数据行：若有表格横线则按横线区间切分（使用字符垂直中心点防越界）。"""
        if not self.col_bounds:
            return []

        page_hdr_bottom = self._detect_page_header_bottom(vpage)
        if page_hdr_bottom > 0:
            cutoff = page_hdr_bottom
        elif is_first_page:
            cutoff = self.hdr_bottom
        else:
            cutoff = 0.0

        # 在提取本页数据行前，先执行通用表尾截断：将『说明：』『重要提示』『本期总笔数』等表外区域彻底切除！
        page_chars = truncate_page_footer_chars(vpage.chars, min_y=cutoff)

        # 选用页面自身的竖线边界（若本页竖线数恰等于列数+1），否则沿用全局/平移列边界
        if len(vpage.vlines_pt) == len(self.col_names) + 1:
            bounds = [
                (vpage.vlines_pt[i], vpage.vlines_pt[i + 1])
                for i in range(len(self.col_names))
            ]
        else:
            all_chars_raw = [c for c in page_chars if c['top'] > cutoff]
            bounds = self._page_col_bounds(all_chars_raw, is_first_page)

        if fmt == 'zones' and len(vpage.hlines_pt) >= 1:
            min_line_y = max(0.0, cutoff - 6.0) if cutoff > 0 else 0.0
            sep_ys = sorted(y for y in vpage.hlines_pt if y >= min_line_y)
            if not sep_ys:
                return self._extract_ocr_text_records(vpage, cutoff, bounds, is_first_page, page_chars=page_chars)
            if cutoff > 0:
                if sep_ys[0] - cutoff > 10.0:
                    zone_bounds = [cutoff] + sep_ys
                else:
                    zone_bounds = sep_ys
            else:
                if len(vpage.vlines_pt) >= 4:
                    zone_bounds = sep_ys
                else:
                    zone_bounds = [0.0] + sep_ys
            if not vpage.vlines_pt and zone_bounds[-1] < vpage.height - 20.0:
                zone_bounds = zone_bounds + [vpage.height + 1.0]
            res = []
            for i in range(len(zone_bounds) - 1):
                y0, y1 = zone_bounds[i], zone_bounds[i + 1]
                if y1 - y0 < 6.0:
                    continue
                zc = [
                    c for c in page_chars
                    if y0 < (c['top'] + c['bottom']) * 0.5 <= y1
                ]
                zc = drop_skip_lines(zc)
                # 若末个区间因底线漏检而高度过大 (> 55pt)，按内部行距 (> 14pt) 自动切离底部非数据区或拆分两条记录
                if zc and (y1 - y0) > 55.0:
                    z_lines = group_by_y(zc, tol=5.0)
                    if len(z_lines) >= 2:
                        kept_z_chars = list(z_lines[0])
                        prev_bot = max(c['bottom'] for c in z_lines[0])
                        for z_rg in z_lines[1:]:
                            cur_top = min(c['top'] for c in z_rg)
                            if cur_top - prev_bot > 14.0:
                                break
                            kept_z_chars.extend(z_rg)
                            prev_bot = max(c['bottom'] for c in z_rg)
                        zc = kept_z_chars
                if zc:
                    row = ocr_chars_to_row(zc, bounds)
                    if (any(row)
                            and not is_skip_row(row)
                            and not self._is_english_row(row)
                            and not self._is_header_row(row)
                            and not self._is_metadata_zone_row(row)):
                        res.append(row)
            return res

        return self._extract_ocr_text_records(vpage, cutoff, bounds, is_first_page, page_chars=page_chars)

    def _extract_ocr_text_records(
            self, vpage: VirtualOCRPage, cutoff: float,
            bounds: list, is_first_page: bool,
            page_chars: Optional[list] = None) -> list:
        """针对无表格线 (text 模式，如农业银行、长沙银行等) 的自适应记录分组提取。"""
        src_chars = page_chars if page_chars is not None else truncate_page_footer_chars(vpage.chars, min_y=cutoff)
        all_chars = [c for c in src_chars if c['top'] > cutoff]
        if not bounds or not all_chars:
            return []

        char_rows = group_by_y(all_chars, tol=5.5)
        if not char_rows:
            return []

        filtered_rows = []
        for rg in char_rows:
            line_text = ''.join(c['text'] for c in sorted(rg, key=lambda c: c['x0']))
            if any(p.search(line_text) for p in UNIVERSAL_SKIP_PATTERNS):
                continue
            if _is_english_header_line(line_text):
                continue
            filtered_rows.append(rg)
        char_rows = filtered_rows
        if not char_rows:
            return []

        if len(char_rows) == 1:
            row = ocr_chars_to_row(char_rows[0], bounds)
            return [row] if (any(row) and not is_skip_row(row)) else []

        row_tops = [min(c['top'] for c in rg) for rg in char_rows]
        gaps = [row_tops[i] - row_tops[i - 1] for i in range(1, len(row_tops))]
        threshold = self._find_gap_threshold(gaps)

        record_groups = [[char_rows[0]]]
        for i in range(1, len(char_rows)):
            is_new_record = (threshold is not None and gaps[i - 1] > threshold)
            preview_row = ocr_chars_to_row(char_rows[i], bounds)
            has_date_now = any(starts_with_date(c) for c in preview_row[:2] if c)
            if not is_new_record and has_date_now:
                is_new_record = True

            if is_new_record:
                # 应对『日期/金额垂直居中、而对手户名/账号跨3行』的无线框排版（如长沙银行）：
                # 仅当 prev_line 不是上一行的短尾缀（如农行『限公司』『款』『入』），且 prev_line
                # 提供了当前日期行缺失、而上一条记录已具备的关键列（如余额/金额/长户名/账号）时，
                # 才将 prev_line 视为当前垂直居中记录的顶行移入当前新记录！
                if has_date_now and len(record_groups[-1]) >= 2 and gaps[i - 1] < 8.5:
                    prev_line = record_groups[-1][-1]
                    prev_preview = ocr_chars_to_row(prev_line, bounds)
                    prev_has_date = any(starts_with_date(c) for c in prev_preview[:2] if c)
                    rest_chars = [c for rg in record_groups[-1][:-1] for c in rg]
                    rest_preview = ocr_chars_to_row(rest_chars, bounds)
                    rest_has_date = any(starts_with_date(c) for c in rest_preview[:2] if c)
                    if not prev_has_date and rest_has_date:
                        non_empty_prev = [c for c in prev_preview if c]
                        # 若 prev_line 仅包含短词尾（如 '限公司'、'有限公司'、'款'、'入'），它必是上一行的换行尾缀，绝不可下移！
                        is_short_tail = (
                            all(len(c) <= 4 for c in non_empty_prev)
                            or any(c in ('限公司', '有限公司', '限有限公司', '公司', '款', '入', '出', '部', '户')
                                   for c in non_empty_prev)
                        )
                        # 检查 prev_line 是否补充了当前行缺失且上一行已有的列（如余额/金额/摘要）
                        complements_cur = sum(
                            1 for ci_k in range(2, len(bounds))
                            if prev_preview[ci_k] and not preview_row[ci_k] and rest_preview[ci_k]
                        )
                        if not is_short_tail and complements_cur >= 1:
                            moved_top_line = record_groups[-1].pop()
                            record_groups.append([moved_top_line, char_rows[i]])
                            continue
                record_groups.append([char_rows[i]])
            else:
                record_groups[-1].append(char_rows[i])

        rows = []
        for group in record_groups:
            merged_chars = [c for rg in group for c in rg]
            row = ocr_chars_to_row(merged_chars, bounds)
            if (any(row)
                    and not is_skip_row(row)
                    and not self._is_english_row(row)
                    and not self._is_header_row(row)
                    and not self._is_metadata_zone_row(row)):
                rows.append(row)
        return rows

    @staticmethod
    def _is_metadata_zone_row(row: list) -> bool:
        """判断一个行区间是否为跨逻辑页穿插的页眉元数据块、页脚汇总块或隐私声明块。

        注意：竖线切分可能把 '电子回单专用章' 拆到相邻两列（如 '电子回单专' | '用章'），
        因此先做无空格拼接再匹配关键词。
        """
        joined_nospace = ''.join(c for c in row if c).replace(' ', '')
        if not joined_nospace:
            return True
        if joined_nospace in ('具体交易明细', '账户明细', '交易明细'):
            return True
        if any(kw in joined_nospace for kw in (
            '交易明细涉及您的', '仅供个人参考', 'AccountName', 'AccountNo',
            'TransferAmountRange', '收支类别：', '转账金额区间：', 'PrintTime',
            '本期总笔数', '期总笔数', '页面总笔数', '总收入金额', '总支出金额',
            '重要提示', '电子回单专用章', '打印回单或账单', '兹证明：', '居民身份证：',
            '交易明细对应时间段', '承上余额', '起止日期：', '起止日期:',
            '仅证明：在用户选择', '仅用于用户申请', '不得用于其他用途', '账户记录为准',
            '零钱的充值和提现', '微信支付交易明细证明》',
        )):
            return True
        return False

    @staticmethod
    def _is_english_row(row: list) -> bool:
        """判断整行是否为残留的纯英文表头翻译行（不含任何中文字符且主要由英文字母构成）。"""
        joined = ''.join(c for c in row if c).replace(' ', '').strip()
        if not joined:
            return False
        if any(ord(c) > 0x2E7F for c in joined):
            return False
        alpha_cnt = sum(1 for c in joined if c.isalpha())
        return alpha_cnt >= 5 and alpha_cnt >= len(joined) * 0.65

    @staticmethod
    def _build_ocr_cols(header_groups: list, page_width: float,
                        vlines_pt: Optional[List[float]] = None) -> Tuple[list, list]:
        """构建 OCR 专属列边界：

        1. 采用更精细的 8.5pt 聚类阈值，防止窄列换行表头（如农商行『摘/要』与『交易渠/道』，
           两列 x0 仅相距 10~11pt）被误聚合成『摘交易渠要道』一列！
        2. 优先采用像素级检测到的表格竖线作为列边界，否则取相邻表头空白中点。
        """
        all_w = sorted(
            [w for g in header_groups for w in g],
            key=lambda w: w['x0']
        )
        if not all_w:
            return [], []

        clusters, cur = [], [all_w[0]]
        for w in all_w[1:]:
            # 若两个词在同一水平行且各自已经是独立的已知表头词，绝不合并！
            same_layer = abs(w['top'] - cur[-1]['top']) < 5.0
            tol = 8.5 if same_layer else 12.0
            if same_layer and w['text'] in UNIVERSAL_HEADER_KW and cur[-1]['text'] in UNIVERSAL_HEADER_KW:
                clusters.append(cur)
                cur = [w]
            elif abs(w['x0'] - cur[-1]['x0']) < tol:
                cur.append(w)
            else:
                clusters.append(cur)
                cur = [w]
        clusters.append(cur)

        raw_cluster_names = []
        for cl in clusters:
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

            layer_texts = []
            for layer in layers:
                layer.sort(key=lambda w: w['x0'])
                text = ''.join(w['text'] for w in layer)
                if text not in layer_texts:
                    layer_texts.append(text)
            raw_cluster_names.append(sanitize(_clean_col_name(''.join(layer_texts))))

        # 先检查相邻两列是否已经分别是『摘要渠』和『交易道』（若是，直接正名，避免重复切分出第12列）
        for ki in range(len(raw_cluster_names) - 1):
            if raw_cluster_names[ki] in ('摘要渠', '摘渠', '要渠') and raw_cluster_names[ki + 1] in ('交易道', '要交易道', '渠道'):
                raw_cluster_names[ki] = '摘要'
                raw_cluster_names[ki + 1] = '交易渠道'

        names = []
        final_clusters = []
        for cl, name in zip(clusters, raw_cluster_names):
            # 修复农商行等窄列双行表头『摘/要』与『交易渠/道』因水平紧挨被混检成一列的问题
            if (('摘' in name or '要' in name)
                    and ('渠' in name or '道' in name)
                    and name not in ('交易摘要',)):
                cx0 = min(w['x0'] for w in cl)
                cx1 = max(w['x1'] for w in cl)
                ctop = min(w['top'] for w in cl)
                cbot = max(w['bottom'] for w in cl)
                sub_cols = ['摘要', '交易渠道']
                if '用途' in name:
                    sub_cols.append('用途')
                if '备注' in name:
                    sub_cols.append('备注')
                step_w = max(cx1 - cx0, 12.0) / len(sub_cols)
                for si, sc_name in enumerate(sub_cols):
                    names.append(sc_name)
                    final_clusters.append([{
                        'text': sc_name,
                        'x0': cx0 + si * step_w,
                        'x1': cx0 + (si + 1) * step_w - 1.0,
                        'top': ctop,
                        'bottom': cbot,
                    }])
            elif '收/支/其他' in name and '交易方式' in name:
                cx0 = min(w['x0'] for w in cl)
                cx1 = max(w['x1'] for w in cl)
                ctop = min(w['top'] for w in cl)
                cbot = max(w['bottom'] for w in cl)
                mid_x = cx0 + (cx1 - cx0) * 0.42
                names.extend(['具体交易明细收/支/其他', '交易方式'])
                final_clusters.append([{'text': '具体交易明细收/支/其他', 'x0': cx0, 'x1': mid_x - 1.0, 'top': ctop, 'bottom': cbot}])
                final_clusters.append([{'text': '交易方式', 'x0': mid_x, 'x1': cx1, 'top': ctop, 'bottom': cbot}])
            else:
                names.append(name)
                final_clusters.append(cl)

        # 微信支付标准 8 列表头兜底对齐：若表头含『交易单号』与『商户单号』且存在 9 条竖线
        if (vlines_pt and len(vlines_pt) == 9
                and any('交易单号' in n for n in names)
                and any('商户单号' in n for n in names)):
            wx_names = ['交易单号', '交易时间', '交易类型', '收/支/其他', '交易方式', '金额(元)', '交易对方', '商户单号']
            wx_bounds = [(vlines_pt[i], vlines_pt[i + 1]) for i in range(8)]
            return wx_names, wx_bounds

        names = [normalize_ocr_header_name(n) for n in names]
        clusters = final_clusters
        if len(names) <= 1:
            return build_cols(header_groups, page_width)

        # 如果图像中检测到的竖线数量恰好等于 列数 + 1，直接使用物理表格竖线作为列边界！
        if vlines_pt and len(vlines_pt) == len(names) + 1:
            exact_bounds = [
                (vlines_pt[i], vlines_pt[i + 1])
                for i in range(len(names))
            ]
            return names, exact_bounds

        col_spans = [
            (min(w['x0'] for w in cl), max(w['x1'] for w in cl))
            for cl in clusters
        ]
        refined_bounds = []
        for i in range(len(col_spans)):
            if i == 0:
                left = max(0.0, col_spans[0][0] - 10.0)
            else:
                # 银行流水无竖线表格（如农商行、农行、长沙银行）普遍采用左对齐排版，
                # 数据从本列表头 x0 处起排并向右延伸至下一列表头 x0 之前！
                # 因此分界线应紧贴下一列表头左缘（留 2.5pt 容差），绝不可取两表头中间点（否则会腰斩左列长文本/长余额）！
                cur_left = col_spans[i][0]
                prev_right = col_spans[i - 1][1]
                margin = min(3.0, max(0.5, (cur_left - prev_right) * 0.2)) if cur_left > prev_right else 2.0
                left = cur_left - margin

            if i + 1 < len(col_spans):
                next_left = col_spans[i + 1][0]
                cur_right = col_spans[i][1]
                margin = min(3.0, max(0.5, (next_left - cur_right) * 0.2)) if next_left > cur_right else 0.5
                right = next_left - margin
            else:
                right = page_width
            refined_bounds.append((left, right))

        return names, refined_bounds

    def _fallback_columns_if_needed(self, vpage: VirtualOCRPage) -> None:
        """当截图不含表头时，按 X 坐标投影切分列边界。"""
        if self.col_bounds or not vpage.chars:
            return
        words = vpage.extract_words()
        if not words:
            return
        sorted_w = sorted(words, key=lambda w: w['x0'])
        clusters, cur = [], [sorted_w[0]]
        for w in sorted_w[1:]:
            if abs(w['x0'] - cur[-1]['x0']) < 20:
                cur.append(w)
            else:
                clusters.append(cur)
                cur = [w]
        clusters.append(cur)
        valid_clusters = [c for c in clusters if len(c) >= 2] or clusters
        bounds = []
        names = []
        for i, cl in enumerate(valid_clusters):
            x0 = min(w['x0'] for w in cl)
            x1 = (valid_clusters[i + 1][0]['x0'] - 1.0
                  if i + 1 < len(valid_clusters) else vpage.width)
            bounds.append((x0 - 4, x1))
            names.append(f"列{i + 1}")
        self.col_names = names
        self.col_bounds = bounds
        self.hdr_bottom = 0.0

    def _filter_non_transaction_rows(self) -> None:
        """代码级过滤与续行挽救：剔除跨页混入的页眉/页尾汇总/重要提示，同时确保绝不丢失表内跨行数据。

        策略：
        1. 命中 _is_metadata_zone_row / _is_english_row / _is_header_row / is_skip_row 的非数据行直接剔除；
        2. 若某行在日期列含合法日期，或借贷/金额/余额列含有有效数值，作为正常交易记录保留；
        3. 若某行既无日期也无金额（如跨横线/跨页换行的长户名或备注尾行），且前面已有交易记录，
           将其非空单元格合并到上一条交易记录中，绝不丢弃表内文本！
        """
        if not self.data_rows:
            return

        date_cols = [
            i for i, name in enumerate(self.col_names)
            if any(kw in name.lower() for kw in ('日期', '时间', 'date', 'time'))
        ]
        if not date_cols:
            # 检查首列是否确实为日期列；若整个表格本身不含日期列（如流水分析报告汇总表），则仅过滤元数据/表头行
            sample_has_date = sum(
                1 for r in self.data_rows[:15]
                if r and r[0] and DATE_CONTENT_RE.search(r[0])
            )
            if sample_has_date >= 2:
                date_cols = [0]
            else:
                self.data_rows = [
                    r for r in self.data_rows
                    if not (self._is_metadata_zone_row(r)
                            or self._is_english_row(r)
                            or self._is_header_row(r)
                            or is_skip_row(r))
                ]
                return

        num_cols = [
            i for i, name in enumerate(self.col_names)
            if any(kw in name for kw in _AMOUNT_KW + _BALANCE_KW)
        ]

        cleaned_rows = []
        for row in self.data_rows:
            if (self._is_metadata_zone_row(row)
                    or self._is_english_row(row)
                    or self._is_header_row(row)
                    or is_skip_row(row)):
                continue

            has_valid_date = any(
                ci < len(row) and row[ci] and DATE_CONTENT_RE.search(row[ci])
                for ci in date_cols
            )
            has_valid_num = any(
                ci < len(row) and row[ci] and _parse_float(clean_ocr_amount(row[ci])) is not None
                for ci in num_cols
            )

            if has_valid_date or has_valid_num:
                cleaned_rows.append(row)
            elif cleaned_rows:
                # 仅当首列为空、非空列不超过 2 列、总字数 <= 18 且不含任何句号/说明字样时，
                # 才视为跨页换行的短词尾合并至上一行；绝不可将表尾说明段落合并进最后一行！
                non_empty_cells = [(j, c.strip()) for j, c in enumerate(row) if c.strip()]
                total_chars = sum(len(v) for _, v in non_empty_cells)
                joined_s = ''.join(v for _, v in non_empty_cells)
                if (not row[0].strip()
                        and 1 <= len(non_empty_cells) <= 2
                        and total_chars <= 18
                        and not any(ch in joined_s for ch in '。；！？《》：:')
                        and not _PAGE_FOOTER_CUTOFF_RE.search(joined_s)):
                    prev = cleaned_rows[-1]
                    for j, val in non_empty_cells:
                        if j < len(prev) and val not in prev[j]:
                            prev[j] = (prev[j] + val).strip()

        self.data_rows = cleaned_rows

    def _extract_meta(self, page, hdr_top: float) -> list:
        """提取表头上方区域的账户元数据信息（保留同行内的『打印日期』『查询时间段』『本方户名』等关键信息）。"""
        wds = [w for w in page.extract_words() if (w['top'] + w['bottom']) * 0.5 < hdr_top]
        if not wds:
            return []
        # 有横线时先在物理行内关联键值，避免双语标签与垂直居中的值被拆散。
        grid_meta = []
        used = set()
        lines = sorted(y for y in getattr(page, 'hlines_pt', []) if y < hdr_top)
        label_re = re.compile(r'^([\u4e00-\u9fff][\u4e00-\u9fff /（）()]+)[：:](.*)$')
        for top, bottom in zip(lines, lines[1:]):
            if bottom - top > 60:
                continue
            band = [w for w in wds if top < (w['top'] + w['bottom']) / 2 < bottom]
            labels = sorted((w for w in band if label_re.match(w['text'])), key=lambda w: w['x0'])
            if not labels or any(b['x0'] - a['x0'] < 20 for a, b in zip(labels, labels[1:])):
                continue
            pairs = []
            for index, label in enumerate(labels):
                end_x = labels[index + 1]['x0'] if index + 1 < len(labels) else page.width
                segment = [w for w in band if label['x0'] - 2 <= w['x0'] < end_x - 2]
                match = label_re.match(label['text'])
                values = [match.group(2).strip()] if match.group(2).strip() else []
                for row in group_by_y(segment, tol=5.5):
                    row = sorted(row, key=lambda w: w['x0'])
                    # 仅剔除以冒号结尾的英文标签前缀，不删除其后的柜员号、日期等值。
                    translation_end = -1
                    for j, word in enumerate(row):
                        prefix = ' '.join(w['text'] for w in row[:j + 1])
                        if re.fullmatch(r"[A-Za-z /.'’()\-]+[:：]", prefix):
                            translation_end = j
                    candidates = [w for j, w in enumerate(row)
                                  if j > translation_end and w is not label]
                    # 大水平间隙分隔不同信息块，页码过滤不能连带删除左侧字段值。
                    chunks = []
                    for word in candidates:
                        if not chunks or word['x0'] - chunks[-1][-1]['x1'] > 24:
                            chunks.append([])
                        chunks[-1].append(word)
                    for chunk in chunks:
                        text = ' '.join(w['text'] for w in chunk)
                        page_counter = re.search(
                            r'查询结果共|此页为第|Query\s*Result\s*Total\s*Page|^Page\s*\d+', text, re.I)
                        if text and not page_counter and not any(p.search(text) for p in UNIVERSAL_SKIP_PATTERNS):
                            values.append(text)
                pairs.append((sanitize(match.group(1)), sanitize(' '.join(values))))
            grid_meta.append((top, pairs))
            used.update(id(w) for w in band)
        wds = [w for w in wds if id(w) not in used]
        meta = []
        meta_skip = [
            p for p in UNIVERSAL_SKIP_PATTERNS
            if p.pattern not in ('打印日期', '本次查询时间')
        ]
        for r in group_by_y(wds, tol=5.5):
            line = ' '.join(w['text'] for w in r)
            if any(p.search(line) for p in meta_skip):
                continue
            plain = line.replace(' ', '')
            if plain and _is_english_header_line(line):
                continue
            tokens = line.split()
            merged = []
            i = 0
            while i < len(tokens):
                if (i + 1 < len(tokens)
                        and len(tokens[i]) <= 2
                        and all('\u4e00' <= c <= '\u9fff' for c in tokens[i])
                        and any(c in tokens[i + 1] for c in ('：', ':'))):
                    merged.append(tokens[i] + tokens[i + 1])
                    i += 2
                else:
                    merged.append(tokens[i])
                    i += 1
            kv_list = []
            for tok in merged:
                if pdf_to_excel.META_SEP.search(tok):
                    parts = pdf_to_excel.META_SEP.split(tok, maxsplit=1)
                    k = sanitize(parts[0].strip())
                    v = sanitize(parts[1].strip()) if len(parts) > 1 else ''
                    kv_list.append([k, v])
                elif kv_list:
                    kv_list[-1][1] = sanitize((kv_list[-1][1] + ' ' + tok).strip())
                else:
                    kv_list.append(['', sanitize(tok)])
            if kv_list:
                meta.append((min(w['top'] for w in r), [(k, v) for k, v in kv_list]))
        return [pairs for _, pairs in sorted(meta + grid_meta, key=lambda item: item[0])]

    def _clean_english_metadata(self) -> None:
        """清除首页表头上方元数据中的纯英文双语翻译行（如 Account Name: / Currency: 等）。"""
        if not self.metadata:
            return
        cleaned_meta = []
        for pairs in self.metadata:
            kept_pairs = []
            for k, v in pairs:
                combined = f"{k} {v}".strip()
                if not combined:
                    continue
                # 若整对键值不含任何中文字符，且包含英文字母（且非纯日期区间），视为英文翻译行剔除
                if not any(ord(c) > 0x2E7F for c in combined):
                    if any(c.isalpha() for c in combined):
                        continue
                kept_pairs.append((k, v))
            if kept_pairs:
                cleaned_meta.append(kept_pairs)
        self.metadata = cleaned_meta

    def _postprocess_ocr_dates(self) -> None:
        """修复 OCR 单元格内日期与时间粘连、横杠缺失以及窄列时间末位换行断裂的问题。

        典型修复：
        - '2026-02-1803:34:2 9' -> '2026-02-18 03:34:29'
        - '2026-01-27 18:20:0'  -> '2026-01-27 18:20:00'
        - '2025-0303 00:00:21'  -> '2025-03-03 00:00:21'
        - '20250314 04:00:02'   -> '2025-03-14 04:00:02'
        """
        if not self.data_rows or not self.col_names:
            return

        date_cols = [
            i for i, name in enumerate(self.col_names)
            if any(kw in name.lower() for kw in ('日期', '时间', 'date', 'time'))
        ]
        if not date_cols:
            date_cols = [0]

        for row in self.data_rows:
            for ci in date_cols:
                if ci >= len(row) or not row[ci]:
                    continue
                s = row[ci].strip()
                # 0. 若日期列以 '-MM-DD' / '026-MM-DD' / '26-MM-DD' 开头且前一列末尾被拼入了年份前缀（如微信支付单号粘连年份），自动归位
                if ci > 0:
                    m_short_yr = re.match(
                        r'^(?:-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])'
                        r'|0?(2\d)-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01]))(.*)$',
                        s,
                    )
                    if m_short_yr:
                        prev_s = row[ci - 1].strip()
                        if s.startswith('-'):
                            m_yr = re.search(r'(20\d{2})$', prev_s)
                            if m_yr:
                                s = m_yr.group(1) + s
                                row[ci - 1] = prev_s[:-4].strip()
                        elif s.startswith('02'):
                            s = '2' + s
                            if prev_s.endswith('2'):
                                row[ci - 1] = prev_s[:-1].strip()
                        elif s.startswith('2'):
                            s = '20' + s
                            if prev_s.endswith('20'):
                                row[ci - 1] = prev_s[:-2].strip()
                # 0b. 若日期时间列混入了非『年月日』的汉字（如微信支付右列首字『扫』『转』压线串入『2026-01-30扫09:31:57』），
                # 将该汉字剥离并归还到右侧相邻列开头！
                m_cn_leak = re.search(r'([\u4e00-\u9fff]+)', s)
                if m_cn_leak and not any(ch in m_cn_leak.group(1) for ch in ('年', '月', '日')):
                    leaked_cn = m_cn_leak.group(1)
                    s = (s[:m_cn_leak.start()] + ' ' + s[m_cn_leak.end():]).strip()
                    if ci + 1 < len(row) and not row[ci + 1].startswith(leaked_cn):
                        row[ci + 1] = leaked_cn + row[ci + 1]
                # 0c. 若日期与时间之间多粘连了1位相邻列数字（如 '2026-01-28819:15:28' 或 '20251125310:56:15'），剥离中间多出的数字
                m_extra_d = re.match(r'^(0?20\d{2}[-/.]?\d{2}[-/.]?\d{2})(\d)(\d{2}:\d{2}(?::\d{2})?)$', s.replace(' ', ''))
                if m_extra_d:
                    d_part = m_extra_d.group(1)
                    if d_part.startswith('020'):
                        d_part = d_part[1:]
                    s = f"{d_part} {m_extra_d.group(3)}"
                    if ci > 0 and row[ci - 1] and row[ci - 1].isdigit():
                        row[ci - 1] = row[ci - 1] + m_extra_d.group(2)
                # 1. 日期与时间之间缺空格补齐：2026-02-1803:34:29 -> 2026-02-18 03:34:29
                s = re.sub(
                    r'(\d{4}[-/.]?\d{2}[-/.]?\d{2})(?=\d{1,2}:)',
                    r'\1 ',
                    s,
                )
                # 2. 日期若漏了全部或部分横杠（如 20241205 10:12 / 2025-1221 / 2025- 1217 / 202503-14），统一规范为 YYYY-MM-DD
                s = re.sub(
                    r'\b(\d{4})[-/.]?(0[1-9]|1[0-2])[-/.]?(0[1-9]|[12]\d|3[01])(?=\s+\d{1,2}:)',
                    r'\1-\2-\3',
                    s,
                )
                s = re.sub(
                    r'(?<!\d)(\d{4})\s*[-/.]\s*(0[1-9]|1[0-2])\s*[-/.]?\s*(0[1-9]|[12]\d|3[01])(?!\d)',
                    r'\1-\2-\3',
                    s,
                )
                s = re.sub(
                    r'(?<!\d)(\d{4})\s*(0[1-9]|1[0-2])\s*[-/.]\s*(0[1-9]|[12]\d|3[01])(?!\d)',
                    r'\1-\2-\3',
                    s,
                )
                # 3. 若单元格内因压线混入前一行残留日期前缀（如 '2026-02-27 2026-02-28 09:29:18'），保留带时间的完整日期
                m_dup = re.search(r'(\d{4}-\d{2}-\d{2}\s+\d{1,2}:.*)$', s)
                if m_dup:
                    s = m_dup.group(1)
                # 4. 时间内部因窄列换行产生的空格粘合：03:34:2 9 -> 03:34:29, 23:09 20 -> 23:09:20
                s = re.sub(
                    r'(\d{1,2}:\d{2}:\d)\s+(\d)(?!\d)',
                    r'\1\2',
                    s,
                )
                s = re.sub(
                    r'(\d{1,2}:\d{2})\s+(\d{2})(?!\d)',
                    r'\1:\2',
                    s,
                )
                s = re.sub(
                    r'(\d{1,2}:\d)\s+(\d:\d{2})',
                    r'\1\2',
                    s,
                )
                # 5. 若窄列换行导致时间末尾秒数 '0' 漏检（如 '18:20:0' 或 '11:51:'），补齐末位 '0'
                s = re.sub(r'(\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}:\d)$', r'\g<1>0', s)
                s = re.sub(r'(\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}:)$', r'\g<1>00', s)
                # 6. 8位纯数字记账日期清除首尾孤立水印噪声：C20251103 / 3 20251015 / 20260203 3 -> 20251103
                if '日期' in self.col_names[ci] and ':' not in s:
                    m8 = re.search(r'(20\d{2}(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01]))', s.replace(' ', ''))
                    if m8 and len(s.replace(' ', '')) <= 10 and '-' not in s:
                        s = m8.group(1)
                row[ci] = s

        # 7. 同列日期格式统一：若某日期列的主体格式为带横杠的 YYYY-MM-DD（占比 >= 15%），
        #    将该列中因横杠较细漏检而成的 8 位纯数字日期（如 20251221）统一补全为 2025-12-21！
        for ci in date_cols:
            col_vals = [r[ci].strip() for r in self.data_rows if ci < len(r) and r[ci].strip()]
            if not col_vals:
                continue
            hyphen_cnt = sum(1 for v in col_vals if re.match(r'^\d{4}-\d{2}-\d{2}', v))
            if hyphen_cnt >= max(2, len(col_vals) * 0.15):
                for r in self.data_rows:
                    if ci < len(r) and r[ci]:
                        r[ci] = re.sub(
                            r'^(20\d{2})(0[1-9]|1[0-2])(0[1-9]|[12]\d|3[01])(?=$|\s)',
                            r'\1-\2-\3',
                            r[ci].strip(),
                        )

    def _postprocess_ocr_numbers(self) -> None:
        """针对金额列、余额列及收支方向列执行 OCR 专属清洗。"""
        if not self.data_rows or not self.col_names:
            return

        num_cols = []
        dir_cols = []
        acct_cols = []
        for i, name in enumerate(self.col_names):
            if any(kw in name for kw in _AMOUNT_KW + _BALANCE_KW):
                num_cols.append(i)
            if name in ('支/收', '收/支', '借/贷', '收支', '借贷', '收支标志', '收支方向'):
                dir_cols.append(i)
            if any(kw in name for kw in ('账号', '卡号', '账户')):
                acct_cols.append(i)

        for row in self.data_rows:
            # 全面清除单元格内因跨行换行或 OCR 框拼接残留的空格与换行符：
            # 仅保留「日期 与 时间」之间的单个空格（如 2025-10-01 04:38:41 或 20251001 04:38:41），
            # 其余所有文字、户名、账号、金额、流水号内部的空格一律无缝拼接！
            for idx_c in range(len(row)):
                if row[idx_c]:
                    cell_s = row[idx_c].replace('\r', '').replace('\n', '')
                    # 保护日期与时间之间的单个空格
                    cell_s = re.sub(
                        r'(\b\d{4}(?:[-/.]\d{1,2}[-/.]\d{1,2}|\d{4}))\s+(\d{1,2}:\d{2})',
                        r'\1__DT_SPACE__\2',
                        cell_s,
                    )
                    cell_s = re.sub(r'\s+', '', cell_s)
                    cell_s = cell_s.replace('__DT_SPACE__', ' ')
                    cell_s = re.sub(r'有限(?:有限)+公司', '有限公司', cell_s)
                    row[idx_c] = cell_s
            for ci in num_cols:
                if ci < len(row) and row[ci]:
                    row[ci] = clean_ocr_amount(row[ci])
                    # 若金额/余额末尾仅有 1 位小数（如 '256.9'），且右侧相邻文字单元格开头恰好串入了第 2 位小数（如 '6网上快捷提现'），自动归位
                    if re.search(r'\.\d$', row[ci]) and (ci + 1) < len(row) and row[ci + 1]:
                        m_leak_dec = re.match(r'^(\d)([\u4e00-\u9fff].*)$', row[ci + 1])
                        if m_leak_dec:
                            row[ci] = row[ci] + m_leak_dec.group(1)
                            row[ci + 1] = m_leak_dec.group(2)
            for ci in acct_cols:
                if ci < len(row) and row[ci]:
                    av = row[ci].strip()
                    # 若是纯账号列（不含中文字符），将 Q/O/l/I 等形近字母修正为数字
                    if av and not any(ord(c) > 0x2E7F for c in av):
                        if sum(1 for c in av if c.isdigit()) >= len(av) * 0.75:
                            row[ci] = av.translate(_OCR_DIGIT_TRANS).replace(' ', '')
            for ci in dir_cols:
                if ci < len(row) and row[ci]:
                    v = row[ci].strip()
                    if v not in ('支', '收', '借', '贷', '支出', '收入'):
                        for d_ch in ('支', '收', '借', '贷'):
                            if d_ch in v:
                                row[ci] = d_ch
                                break

    def _verify_and_repair_balances(self) -> None:
        """利用借贷发生额与余额的钩稽关系校验并自动修复 OCR 小数点/千分位误识。

        同时支持正序（从旧到新：上一行余额 + 本行发生额 = 本行余额）
        与倒序（从新到旧，如兴业银行：下一行余额 + 本行发生额 = 本行余额）两种流水排序！
        """
        if not self.data_rows or len(self.data_rows) < 2 or not self.col_names:
            return

        bal_ci = -1
        debit_ci = -1
        credit_ci = -1
        amt_ci = -1
        dir_ci = -1

        for i, name in enumerate(self.col_names):
            if bal_ci < 0 and any(kw in name for kw in _BALANCE_KW):
                bal_ci = i
            elif debit_ci < 0 and any(kw in name for kw in ('借方', '支出', '转出', '出账')):
                debit_ci = i
            elif credit_ci < 0 and any(kw in name for kw in ('贷方', '收入', '转入', '入账')):
                credit_ci = i
            elif amt_ci < 0 and any(kw in name for kw in _AMOUNT_KW):
                amt_ci = i
            if dir_ci < 0 and name in ('支/收', '收/支', '借/贷', '收支', '借贷', '收支标志', '收支方向'):
                dir_ci = i

        # 若同时存在独立的借方列与贷方列（如建行、农商行、兴业公户），则禁用单列 amt_ci 覆写，
        # 绝不可把借方列或贷方列当成带正负号的单列『交易金额』去覆盖！
        if debit_ci >= 0 and credit_ci >= 0:
            amt_ci = -1

        # 若存在『支/收』方向列且某行因压线漏识别为空，根据交易金额正负号自动补全
        if dir_ci >= 0 and amt_ci >= 0:
            for row in self.data_rows:
                if dir_ci < len(row) and not row[dir_ci].strip() and amt_ci < len(row):
                    aval = _parse_float(row[amt_ci])
                    if aval is not None and aval != 0:
                        row[dir_ci] = '支' if aval < 0 else '收'

        if bal_ci < 0:
            return

        def _row_net_amount(row: list) -> Optional[float]:
            if debit_ci >= 0 or credit_ci >= 0:
                d_val = (_parse_float(row[debit_ci])
                         if 0 <= debit_ci < len(row) else None) or 0.0
                c_val = (_parse_float(row[credit_ci])
                         if 0 <= credit_ci < len(row) else None) or 0.0
                return round(c_val - d_val, 2)
            if 0 <= amt_ci < len(row):
                aval = _parse_float(row[amt_ci])
                if aval is not None and dir_ci >= 0 and dir_ci < len(row):
                    if row[dir_ci].strip() in ('支', '支出', '借') and aval > 0:
                        return -aval
                return aval
            return None

        # 自动探测流水是正序（升序）还是倒序（降序，如兴业银行最新记录在最前）
        asc_matches = 0
        desc_matches = 0
        checked_pairs = 0
        for idx in range(1, min(len(self.data_rows), 20)):
            b_prev = _parse_float(self.data_rows[idx - 1][bal_ci]) if bal_ci < len(self.data_rows[idx - 1]) else None
            b_cur = _parse_float(self.data_rows[idx][bal_ci]) if bal_ci < len(self.data_rows[idx]) else None
            if b_prev is None or b_cur is None:
                continue
            checked_pairs += 1
            a_cur = _row_net_amount(self.data_rows[idx])
            a_prev = _row_net_amount(self.data_rows[idx - 1])
            if a_cur is not None and abs((b_cur - b_prev) - a_cur) <= 0.02:
                asc_matches += 1
            if a_prev is not None and abs((b_prev - b_cur) - a_prev) <= 0.02:
                desc_matches += 1

        # 若该流水按收支筛选（如仅导出『收入』，导致连续行之间余额天然不连续，匹配率 < 45%），
        # 则跳过基于连续余额方程的强制覆写，防止误改筛选流水中的正常余额！
        if checked_pairs >= 4 and max(asc_matches, desc_matches) < checked_pairs * 0.45:
            return

        is_descending = desc_matches > asc_matches
        repaired_count = 0
        warn_rows = []

        # 按时间先后顺序（从旧到新）遍历修复，确保当连续多行被印章遮挡时，已修复的前序余额能立即作为下一行的基准！
        iter_indices = (
            range(len(self.data_rows) - 1, 0, -1)
            if is_descending else range(1, len(self.data_rows))
        )
        for idx in iter_indices:
            if is_descending:
                older_row = self.data_rows[idx]
                newer_row = self.data_rows[idx - 1]
                target_row_idx = idx - 1
            else:
                older_row = self.data_rows[idx - 1]
                newer_row = self.data_rows[idx]
                target_row_idx = idx

            if bal_ci >= len(older_row) or bal_ci >= len(newer_row):
                continue

            older_bal = _parse_float(older_row[bal_ci])
            newer_bal = _parse_float(newer_row[bal_ci])
            net_amt = _row_net_amount(newer_row)

            # 若 older_bal 已知且本行发生额 net_amt 已知，检查是否能通过向后 1~4 行的锚点余额
            # 桥接修复本行（及连续被红章遮挡的多行）残缺余额（如 ', 386.87' 或 '65,” 17'）
            if older_bal is not None and net_amt is not None:
                if newer_bal is None or abs((newer_bal - older_bal) - net_amt) > 0.015:
                    bridged = False
                    step_dir = -1 if is_descending else 1
                    base_idx = idx if is_descending else idx - 1
                    running_bal = older_bal
                    path_indices = []
                    for step in range(1, 6):
                        cur_i = base_idx + step * step_dir
                        if not (0 <= cur_i < len(self.data_rows)):
                            break
                        r_step = self.data_rows[cur_i]
                        a_step = _row_net_amount(r_step)
                        if a_step is None:
                            break
                        running_bal = round(running_bal + a_step, 2)
                        path_indices.append((cur_i, running_bal))
                        b_step = _parse_float(r_step[bal_ci]) if bal_ci < len(r_step) else None
                        if step >= 2 and b_step is not None and abs(running_bal - b_step) <= 0.02:
                            # 桥接方程精确成立！将中间所有被印章遮挡损坏的余额还原
                            for fix_i, fix_bal in path_indices[:-1]:
                                self.data_rows[fix_i][bal_ci] = _format_like(fix_bal, older_row[bal_ci])
                                repaired_count += 1
                            newer_bal = _parse_float(newer_row[bal_ci])
                            bridged = True
                            break
                    if bridged:
                        continue

            if older_bal is None or newer_bal is None or net_amt is None:
                continue

            actual_diff = round(newer_bal - older_bal, 2)

            if (abs(actual_diff - net_amt) <= 0.015
                    or abs(abs(actual_diff) - abs(net_amt)) <= 0.015):
                # 若数值一致但漏了负号（例如支/收列为'支'，但交易金额漏识别了负号 '-'）
                if 0 <= amt_ci < len(newer_row) and actual_diff < 0 and not newer_row[amt_ci].strip().startswith('-'):
                    if dir_ci >= 0 and dir_ci < len(newer_row) and newer_row[dir_ci].strip() in ('支', '支出'):
                        newer_row[amt_ci] = '-' + newer_row[amt_ci].strip()
                        repaired_count += 1
                continue

            # 修复1：检查是否因漏识别小数点导致金额放大了 100 倍
            if 0 <= amt_ci < len(newer_row) and abs(abs(net_amt) / 100.0 - abs(actual_diff)) <= 0.02:
                fixed = math.copysign(abs(actual_diff), actual_diff)
                newer_row[amt_ci] = _format_like(fixed, newer_row[amt_ci])
                repaired_count += 1
                continue

            # 修复2：检查是否因数字紧贴表格左竖线导致首位数字/负号被截断
            # （如 '-19,832.00' 被识别为 '9,832.00'，或建行借方 '3,600,000.00' 首字串入左列变成 ',600,000.00'）
            if 0 <= amt_ci < len(newer_row):
                diff_str = f"{abs(actual_diff):.2f}"
                ocr_str = f"{abs(net_amt):.2f}"
                if len(diff_str) > len(ocr_str) and diff_str.endswith(ocr_str):
                    fixed = actual_diff
                    newer_row[amt_ci] = _format_like(fixed, newer_row[amt_ci])
                    repaired_count += 1
                    continue
            elif debit_ci >= 0 or credit_ci >= 0:
                if actual_diff < 0 and 0 <= debit_ci < len(newer_row):
                    raw_d = newer_row[debit_ci].strip()
                    diff_str = f"{abs(actual_diff):.2f}"
                    ocr_str = f"{abs(net_amt):.2f}"
                    if raw_d.startswith(',') or (len(diff_str) > len(ocr_str) and diff_str.endswith(ocr_str)):
                        newer_row[debit_ci] = _format_like(abs(actual_diff), older_row[bal_ci])
                        repaired_count += 1
                        continue
                elif actual_diff > 0 and 0 <= credit_ci < len(newer_row):
                    raw_c = newer_row[credit_ci].strip()
                    diff_str = f"{abs(actual_diff):.2f}"
                    ocr_str = f"{abs(net_amt):.2f}"
                    if raw_c.startswith(',') or (len(diff_str) > len(ocr_str) and diff_str.endswith(ocr_str)):
                        newer_row[credit_ci] = _format_like(abs(actual_diff), older_row[bal_ci])
                        repaired_count += 1
                        continue

            # 修复3：利用跨行桥接方程修复单行余额 (账户余额) 及借/贷分列金额同时因跨竖线粘连造成的误识
            # （例如建行中 贷方 '750,000.00' 与 余额 '2,933,672.14' 紧贴竖线粘连，导致余额首位 '2' 串入贷方）
            next_idx = idx - 2 if is_descending else idx + 1
            if 0 <= next_idx < len(self.data_rows):
                next_row = self.data_rows[next_idx]
                next_bal = _parse_float(next_row[bal_ci]) if bal_ci < len(next_row) else None
                next_amt = _row_net_amount(next_row)
                if next_bal is not None and next_amt is not None:
                    true_newer_bal = round(next_bal - next_amt, 2)
                    true_newer_amt = round(true_newer_bal - older_bal, 2)
                    # 子情况 3A：本行发生额 net_amt 准确，仅本行余额 newer_bal 识别有误
                    if net_amt is not None and abs(net_amt - true_newer_amt) <= 0.02:
                        newer_row[bal_ci] = _format_like(true_newer_bal, older_row[bal_ci])
                        repaired_count += 1
                        continue
                    # 子情况 3B：本行余额首位数字串入左侧紧邻的贷方/借方列（导致余额如 ',933,62.14' 以逗号开头残缺）
                    raw_bal_str = newer_row[bal_ci].strip()
                    if raw_bal_str.startswith(','):
                        newer_row[bal_ci] = _format_like(true_newer_bal, older_row[bal_ci])
                        if true_newer_amt > 0 and 0 <= credit_ci < len(newer_row):
                            newer_row[credit_ci] = _format_like(true_newer_amt, newer_row[credit_ci])
                        elif true_newer_amt < 0 and 0 <= debit_ci < len(newer_row):
                            newer_row[debit_ci] = _format_like(abs(true_newer_amt), newer_row[debit_ci])
                        repaired_count += 1
                        continue

            warn_rows.append(target_row_idx + 1)

        if repaired_count > 0:
            logger.info(
                f"余额钩稽核验：已自动修正 {repaired_count} 处因小数点/千分位模糊导致的金额偏差")
        if warn_rows:
            sample_str = ', '.join(str(r) for r in warn_rows[:8])
            more = f" 等共 {len(warn_rows)} 行" if len(warn_rows) > 8 else ""
            logger.warning(
                f"余额钩稽提示：第 {sample_str}{more} 数据行的发生额与前后余额差不一致，建议人工复核")

    def _unify_counterparty_accounts_and_names(self) -> None:
        """利用同一份流水中交易对手『户名 <-> 账号』的上下文重现规律，自动修复偶发的压线截断字符。

        例如：
        1. 同一对手户名（如『广东兰格钢铁贸易有限公司』）在多行出现标准账号 '44481601040011598'，
           而某一行因紧贴左竖线被截断为 '4481601040011598' 或 '14481601040011598'；
        2. 某行对手户名首字因紧贴竖线缺失（如『东兰格钢铁贸易有限公司』），但其账号后 12 位与
           『广东兰格钢铁贸易有限公司』完全吻合。
        """
        if not self.data_rows or len(self.data_rows) < 2 or not self.col_names:
            return

        acct_ci = -1
        name_ci = -1
        for i, cname in enumerate(self.col_names):
            if acct_ci < 0 and any(k in cname for k in ('对方账号', '对方账户', '对方卡号', '对手账号')):
                if '户名' not in cname and '银行' not in cname:
                    acct_ci = i
            if name_ci < 0 and any(k in cname for k in ('对方户名', '对方账户名', '对方单位', '交易对方', '对手信息')):
                if '账号' not in cname:
                    name_ci = i

        if acct_ci < 0 or name_ci < 0:
            return

        from collections import Counter
        name_to_accts: Dict[str, Counter] = {}
        for row in self.data_rows:
            if max(acct_ci, name_ci) >= len(row):
                continue
            nm = row[name_ci].strip()
            ac = row[acct_ci].strip()
            if len(nm) >= 4 and len(ac) >= 10 and ac.isdigit():
                name_to_accts.setdefault(nm, Counter())[ac] += 1

        # 规则 1：若某行户名首字缺失（如 '东兰格钢铁贸易有限公司' 是 '广东兰格钢铁贸易有限公司' 的后缀，哪怕出现了多次），
        #         且其账号后 10 位与长户名的账号一致，先合并归入完整户名！
        all_names_sorted = sorted(name_to_accts.keys(), key=len, reverse=True)
        name_alias_map: Dict[str, str] = {}
        for short_nm in all_names_sorted:
            for long_nm in all_names_sorted:
                if len(long_nm) - len(short_nm) in (1, 2) and long_nm.endswith(short_nm):
                    short_ac = max(name_to_accts[short_nm].keys(), key=len)
                    long_ac = max(name_to_accts[long_nm].keys(), key=len)
                    if len(short_ac) >= 10 and len(long_ac) >= 10 and short_ac[-10:] == long_ac[-10:]:
                        name_alias_map[short_nm] = long_nm
                        name_to_accts[long_nm].update(name_to_accts[short_nm])
                        break

        # 确定每个户名对应的权威账号（优先取长度最完整、且非压线产生的 '1' 开头假长号）
        canon_acct_by_name: Dict[str, str] = {}
        for nm, cnts in name_to_accts.items():
            best_ac = max(cnts.keys(), key=lambda a: (len(a), cnts[a]))
            canon_acct_by_name[nm] = best_ac

        for row in self.data_rows:
            if max(acct_ci, name_ci) >= len(row):
                continue
            nm = row[name_ci].strip()
            if nm in name_alias_map:
                row[name_ci] = name_alias_map[nm]
                nm = row[name_ci]
            ac = row[acct_ci].strip()
            if nm in canon_acct_by_name and ac and ac != canon_acct_by_name[nm]:
                c_ac = canon_acct_by_name[nm]
                if len(c_ac) >= 12 and len(ac) >= 10:
                    # 首尾截断 1~2 位，或仅首位 1 个数字不同（其余完全相同）
                    if (c_ac.endswith(ac)
                            or c_ac.startswith(ac)
                            or c_ac[1:-1] == ac
                            or (len(c_ac) == len(ac) and c_ac[1:] == ac[1:])):
                        row[acct_ci] = c_ac


# ═══════════════════════════════════════════════════════
# 入口函数（与 pdf_to_excel.process_one 签名完全兼容）
# ═══════════════════════════════════════════════════════

def process_one(file_path: str, out_dir: str = None,
                template_path: str = None,
                progress_callback: Optional[Callable[[int, int], None]] = None,
                cancel_check: Optional[Callable[[], bool]] = None,
                password: Optional[str] = None) -> bool:
    """使用 OCR 引擎处理单个扫描件 PDF 或流水图片，并输出 Excel。"""
    try:
        p = Path(file_path)
        if not p.exists():
            logger.error(f"文件不存在: {p}")
            return False
        od = Path(out_dir) if out_dir else p.parent
        od.mkdir(parents=True, exist_ok=True)
        out = str(od / (p.stem + '.xlsx'))

        parser = BankOCRParser(
            str(p),
            password=password,
            progress_callback=progress_callback,
            cancel_check=cancel_check,
        ).parse()

        write_excel(parser, out)
        return True
    except Exception as e:
        if _is_encryption_error(e) or 'password' in str(e).lower():
            logger.error(
                f"OCR 处理失败 {Path(file_path).name}: "
                f"PDF 已加密，未提供密码或密码不正确（可通过 -p 参数指定密码）")
            return False
        logger.error(f"OCR 处理失败 {Path(file_path).name}: {e}")
        import traceback
        logger.error(traceback.format_exc())
        return False


def main():
    ap = argparse.ArgumentParser(
        description='银行流水 OCR (扫描件 PDF / 图片) -> Excel 智能转换器')
    ap.add_argument('input', help='扫描件 PDF、图片文件或目录路径')
    ap.add_argument('-o', '--output', default=None, help='输出目录')
    ap.add_argument('-r', '--recursive', action='store_true',
                    help='递归处理子目录')
    ap.add_argument('-p', '--password', default=None,
                    help='加密 PDF 的打开密码')
    args = ap.parse_args()

    ip = Path(args.input)
    if ip.is_file():
        ok = process_one(str(ip), args.output, password=args.password)
        sys.exit(0 if ok else 1)
    elif ip.is_dir():
        pattern = '**/*' if args.recursive else '*'
        files = [
            f for f in ip.glob(pattern)
            if f.is_file() and f.suffix.lower() in OCR_SUPPORTED_EXTENSIONS
        ]
        ok = fail = 0
        for f in files:
            if process_one(str(f), args.output, password=args.password):
                ok += 1
            else:
                fail += 1
        logger.info(f'OCR 批量完成: 成功 {ok}, 失败 {fail}')
        sys.exit(0 if fail == 0 else 1)
    else:
        logger.error(f'路径不存在: {ip}')
        sys.exit(1)


if __name__ == '__main__':
    main()
