"""Offline tests for the library OPAC client.

Every network call is faked: these tests never touch opac.bupt.edu.cn.
"""

import pytest

from bupt_cli import library


def test_clean_text_strips_highlight_and_whitespace():
    raw = "\n\n  <b>机器学习方法</b>\n\n"
    assert library.clean_text(raw) == "机器学习方法"


def test_clean_text_unescapes_entities():
    assert library.clean_text("A &amp; B") == "A & B"


def test_book_record_maps_fields():
    record = library.book_record(
        {
            "recCtrlId": "01h0170785",
            "title": "<b>机器学习方法</b>",
            "authors": "蒋艳凰",
            "isn": "9787121090059",
            "publisher": "电子工业出版社",
            "pubdateDate": "2009",
            "bookSearchNo": "TP181/J385",
            "libraryName": "北京邮电大学图书馆",
            "guancangCount": 5,
            "kejieCount": 2,
        }
    )
    assert record["id"] == "01h0170785"
    assert record["title"] == "机器学习方法"
    assert record["holding_count"] == 5
    assert record["available_count"] == 2


def test_book_record_tolerates_missing_and_bad_counts():
    record = library.book_record({"recCtrlId": "x"})
    assert record["holding_count"] is None
    assert record["available_count"] is None
    assert record["title"] == ""


def test_row_status_picks_status_column():
    cells = [
        "沙河：二层文献借阅区",
        "21113002404231",
        "TP181/J385",
        "1296588",
        "架位导航",
        "",
        "在架可借",
        "异地预借",
        "",
    ]
    assert library._row_status(cells) == "在架可借"


def test_row_status_falls_back_when_column_shifts():
    cells = ["沙河：借阅区", "123", "TP1", "1", "架位导航", "借出"]
    assert library._row_status(cells) == "借出"


def test_row_status_empty_when_no_status_word():
    assert library._row_status(["沙河：借阅区", "123"]) == ""


def test_search_rejects_blank_keyword():
    with pytest.raises(library.LibraryError):
        library.search("   ")


def test_holdings_rejects_blank_id():
    with pytest.raises(library.LibraryError):
        library.holdings("  ")


def _patch_client(monkeypatch, *, post=None, get=None):
    """Replace library._client with a stub returning fixed payloads."""

    class FakeResponse:
        def __init__(self, *, json_data=None, text="", status_code=200):
            self._json = json_data
            self.text = text
            self.status_code = status_code

        def raise_for_status(self):
            if self.status_code >= 400:
                raise RuntimeError(f"HTTP {self.status_code}")

        def json(self):
            return self._json

    class FakeClient:
        def __init__(self, *a, **kw):
            self.calls = []

        def __enter__(self):
            return self

        def __exit__(self, *a):
            return False

        def post(self, url, data=None):
            self.calls.append(("POST", url, data))
            return post

        def get(self, url, headers=None):
            self.calls.append(("GET", url, headers))
            return get

    monkeypatch.setattr(library, "_client", lambda timeout: FakeClient())


def test_search_sends_required_fields(monkeypatch):
    """The three fields the server silently requires (else total=0)."""
    seen = {}

    class Resp:
        status_code = 200
        text = ""

        def raise_for_status(self):
            pass

        def json(self):
            return {
                "result": {"code": 0},
                "total": 1,
                "pageNo": 1,
                "totalPage": 1,
                "data": [{"recCtrlId": "a", "title": "T"}],
            }

    class C:
        def __enter__(self):
            return self

        def __exit__(self, *a):
            return False

        def post(self, url, data=None):
            seen["data"] = data
            return Resp()

    monkeypatch.setattr(library, "_client", lambda timeout: C())
    result = library.search("机器学习", page_size=3)

    assert result["total"] == 1
    assert result["books"][0]["title"] == "T"
    assert seen["data"]["solrType"] == "TITLE"
    assert seen["data"]["xiaoqu"] == "all"


def test_search_raises_on_error_code(monkeypatch):
    class Resp:
        status_code = 200
        text = ""

        def raise_for_status(self):
            pass

        def json(self):
            return {"result": {"code": 1, "msg": "boom"}}

    class C:
        def __enter__(self):
            return self

        def __exit__(self, *a):
            return False

        def post(self, url, data=None):
            return Resp()

    monkeypatch.setattr(library, "_client", lambda timeout: C())
    with pytest.raises(library.LibraryError):
        library.search("x")


def test_electronic_resources_parses_flat_list(monkeypatch):
    """`data` is a flat list of {locate, recNote}; \\x1e must be stripped."""

    class Resp:
        status_code = 200
        text = ""

        def raise_for_status(self):
            pass

        def json(self):
            return {
                "result": {"code": 0},
                "data": [
                    {"locate": "http://x/book\u001e", "recNote": "Electronic version"},
                    {"locate": "http://x/book", "recNote": "\u7535\u5b50\u4e66"},
                ],
            }

    class C:
        def __enter__(self):
            return self

        def __exit__(self, *a):
            return False

        def post(self, url, data=None, headers=None):
            return Resp()

    monkeypatch.setattr(library, "_client", lambda timeout: C())
    result = library.electronic_resources("22h0459422")

    assert result["has_electronic"] is True
    assert result["count"] == 2
    assert result["resources"][0]["url"] == "http://x/book"
    assert result["resources"][1]["label"] == "\u7535\u5b50\u4e66"


def test_electronic_resources_empty_means_none(monkeypatch):
    class Resp:
        status_code = 200
        text = ""

        def raise_for_status(self):
            pass

        def json(self):
            return {"result": {"code": 0}, "data": []}

    class C:
        def __enter__(self):
            return self

        def __exit__(self, *a):
            return False

        def post(self, url, data=None, headers=None):
            return Resp()

    monkeypatch.setattr(library, "_client", lambda timeout: C())
    result = library.electronic_resources("x")

    assert result["has_electronic"] is False
    assert result["count"] == 0


def test_electronic_resources_rejects_blank_id():
    with pytest.raises(library.LibraryError):
        library.electronic_resources("  ")
