"""验证金额可计算，同时保护账号和无法识别的原始值。"""

import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

from openpyxl import load_workbook
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from bankflow.parsers.pdf_to_excel import write_excel


class ExcelValuesTest(unittest.TestCase):
    def test_money_and_identifiers_survive_export(self):
        parser = SimpleNamespace(
            metadata=[], col_names=['收入', '支出', '账户余额', '对方账号'],
            data_rows=[['1,234.56', '0.00', '-20.50', '00123456789012345678'],
                       ['', '无法识别', '12,34.00', '00001']])
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'result.xlsx'
            write_excel(parser, str(path))
            workbook = load_workbook(path)
            sheet = workbook.active
            self.assertEqual([sheet.cell(2, i).value for i in range(1, 5)],
                             [1234.56, 0, -20.5, '00123456789012345678'])
            self.assertEqual(sheet['A2'].data_type, 'n')
            self.assertEqual(sheet['A2'].number_format, '#,##0.00')
            self.assertIsNone(sheet['A3'].value)
            self.assertEqual(sheet['B3'].value, '无法识别')
            self.assertEqual(sheet['C3'].value, '12,34.00')
            workbook.close()
