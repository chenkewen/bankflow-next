"""验证金额可计算，同时保护账号和无法识别的原始值。"""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

from openpyxl import load_workbook
ROOT = Path(__file__).resolve().parents[1]
DEBUG_EXE = ROOT / "src-tauri/target/debug/bankflow-workbench.exe"
DIAGNOSTICS = os.environ.get("BANKFLOW_DIAGNOSTICS_ENGINE") or os.environ.get("BANKFLOW_ROWS_ENGINE") or DEBUG_EXE


class ExcelValuesTest(unittest.TestCase):
    def test_money_and_identifiers_survive_export(self):
        payload = dict(
            metadata=[], columns=['收入', '支出', '账户余额', '对方账号'],
            rows=[['1,234.56', '0.00', '-20.50', '00123456789012345678'],
                       ['', '无法识别', '12,34.00', '00001']])
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'result.xlsx'
            payload["output"] = str(path)
            subprocess.run([DIAGNOSTICS, "--statement-excel"], input=json.dumps(payload).encode(),
                           check=True, stdout=subprocess.PIPE)
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

    def test_payment_bilingual_and_english_amounts_keep_numeric_types(self):
        amounts = [
            '金额(元)', '金额（元）', '借方发生额（支取）', '贷方发生额(收入)',
            '转入金额', '转出金额', '收入/支出金额', '本次余额', '交易余额',
            '交易⾦额 Transaction Amount', '账户余额 Account Balance',
            'Credit', 'Debit', 'Balance', 'Amount', 'Account Balance',
        ]
        identifiers = ['交易单号', '商户单号', '对方账号', '支/收 Expenditure/Income', 'Credit reference']
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'amount-types.xlsx'
            payload = dict(output=str(path), metadata=[], columns=amounts + identifiers, rows=[
                ['-1,234.50'] * len(amounts) + ['00001234'] * len(identifiers),
                ['12345678901234.56'] + [''] * (len(amounts) - 1) + ['00001234567890123456'],
            ])
            subprocess.run([DIAGNOSTICS, '--statement-excel'], input=json.dumps(payload).encode(),
                           check=True, stdout=subprocess.PIPE)
            workbook = load_workbook(path)
            try:
                row = workbook.active[2]
                for header, cell in zip(amounts, row):
                    with self.subTest(header=header):
                        self.assertEqual((cell.data_type, cell.value, cell.number_format),
                                         ('n', -1234.5, '#,##0.00'))
                for header, cell in zip(identifiers, row[len(amounts):]):
                    with self.subTest(header=header):
                        self.assertEqual((cell.data_type, cell.value), ('s', '00001234'))
                self.assertEqual(workbook.active['A3'].value, '12345678901234.56')
                self.assertEqual(workbook.active.cell(3, len(amounts) + 1).value, '00001234567890123456')
            finally:
                workbook.close()
