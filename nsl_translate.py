# nsl_translate.py — 게임 설명 번역 (TranslateMixin) (PySide6)
from PySide6.QtGui import QTextCursor, QTextCharFormat
from PySide6.QtWidgets import QMessageBox


class TranslateMixin:

    # ── 번역 실행 ──────────────────────────────────────────────────────
    def _translate_description(self):
        item = self._get_item()
        if not item:
            return
        desc = item.get('description') or ''
        if not desc:
            QMessageBox.information(self, '알림', '번역할 게임 설명이 없습니다.')
            return

        self._set_status('게임 설명 번역 중...')
        self._translate_bar.hide()

        def worker():
            try:
                import textwrap
                from deep_translator import GoogleTranslator

                translator = GoogleTranslator(source='auto', target='ko')
                chunks = textwrap.wrap(desc, width=4500,
                                       replace_whitespace=False)
                translated = '\n'.join(translator.translate(c) for c in chunks)

                self._invoke_main(
                    lambda: self._replace_translated_desc(translated, desc))

            except ImportError:
                self._invoke_main(lambda: (
                    QMessageBox.critical(self, '오류',
                        '번역 모듈이 설치되어 있지 않습니다.\n'
                        '관리자에게 문의해 주세요.'),
                    self._translate_bar.show(),
                    self._reposition_translate_bar(),
                ))
            except Exception as e:
                err = str(e)
                self._invoke_main(lambda: (
                    QMessageBox.critical(self, '번역 실패',
                        f'오류가 발생했습니다: {err}'),
                    self._translate_bar.show(),
                    self._reposition_translate_bar(),
                ))
            finally:
                self._invoke_main(lambda: self._set_status('대기'))

        self._run_in_thread(worker)

    def _replace_translated_desc(self, translated_text: str,
                                 original_text: str = ''):
        """번역된 본문으로 설명 영역만 교체."""
        if self._desc_pos_start is None or self._desc_pos_end is None:
            return
        cursor = self._detail_info.textCursor()
        try:
            cursor.setPosition(self._desc_pos_start)
            cursor.setPosition(self._desc_pos_end, QTextCursor.KeepAnchor)
            cursor.removeSelectedText()
            content = translated_text.replace('\\n', '\n') + '\n\n'
            cursor.insertText(content, QTextCharFormat())
            self._desc_pos_end = cursor.position()
        except Exception:
            pass
        self._detail_info.verticalScrollBar().setValue(0)
        self._translate_bar.hide()
