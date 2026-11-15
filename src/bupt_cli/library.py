"""Library OPAC (opac.bupt.edu.cn) search and holdings.

Anonymous: no CAS credentials are needed or sent. Two public endpoints:

* ``POST /search-full-result.json`` — bibliographic search. The server answers
  HTTP 200 with ``total: 0`` unless the request carries
  ``X-Requested-With: XMLHttpRequest``, ``solrType=TITLE`` and
  ``xiaoqu=all``; verified against a recorded browser session (2026-11).
* ``GET /bookInfo_<recCtrlId>.html`` — a server-rendered holdings table
  (馆藏部门 / 图书条码 / 索书号 / 登录号 / 状态).

Titles come back HTML-escaped with ``<b>`` match highlighting, so every
string is stripped of tags before it reaches the JSON output.
"""

from __future__ import annotations

import html
import re
from typing import Any, Optional

import httpx

OPAC_BASE = "https://opac.bupt.edu.cn"
SEARCH_URL = f"{OPAC_BASE}/search-full-result.json"
REFERER = f"{OPAC_BASE}/search-full-result.html"

# The OPAC rejects requests without a browser-shaped UA.
_USER_AGENT = (
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36"
)

_TAG_RE = re.compile(r"<[^>]+>")
_WS_RE = re.compile(r"\s+")


class LibraryError(Exception):
    """Raised when the OPAC answers with an unusable payload."""


def clean_text(value: Any) -> str:
    """Strip match-highlight tags and collapse whitespace."""
    if value is None:
        return ""
    text = _TAG_RE.sub("", str(value))
    return _WS_RE.sub(" ", html.unescape(text)).strip()


def _client(timeout: float) -> httpx.Client:
    return httpx.Client(
        timeout=timeout,
        follow_redirects=True,
        headers={
            "User-Agent": _USER_AGENT,
            "Referer": REFERER,
            "X-Requested-With": "XMLHttpRequest",
            "Content-Type": "application/x-www-form-urlencoded; charset=UTF-8",
        },
    )


def search(
    keyword: str,
    *,
    page: int = 1,
    page_size: int = 10,
    timeout: float = 20.0,
    location: str = "all",
    solr_type: str = "TITLE",
) -> dict[str, Any]:
    """Search the OPAC; returns the page of cleaned book records."""
    keyword = keyword.strip()
    if not keyword:
        raise LibraryError("关键词不能为空")

    data = {
        "xiaoqu": location,
        "keyword": keyword,
        "solrType": solr_type,
        "pageNo": str(page),
        "pageSize": str(page_size),
        "classSearch": "0",
        "searchPrecision": "",
    }
    with _client(timeout) as client:
        resp = client.post(SEARCH_URL, data=data)
        resp.raise_for_status()
        payload = resp.json()

    result = payload.get("result") or {}
    if result.get("code") not in (0, "0"):
        raise LibraryError(f"OPAC 检索失败: {result.get('msg') or 'unknown error'}")

    books = [book_record(item) for item in (payload.get("data") or [])]
    return {
        "keyword": keyword,
        "total": int(payload.get("total") or 0),
        "page": int(payload.get("pageNo") or page),
        "total_page": int(payload.get("totalPage") or 0),
        "count": len(books),
        "books": books,
    }


def book_record(item: dict[str, Any]) -> dict[str, Any]:
    """Stable JSON shape for one bibliographic record."""
    return {
        "id": clean_text(item.get("recCtrlId")),
        "title": clean_text(item.get("title")),
        "authors": clean_text(item.get("authors")),
        "isbn": clean_text(item.get("isn")),
        "publisher": clean_text(item.get("publisher")),
        "year": clean_text(item.get("pubdateDate")),
        "call_number": clean_text(item.get("bookSearchNo")),
        "library": clean_text(item.get("libraryName")),
        "holding_count": _int_or_none(item.get("guancangCount")),
        "available_count": _int_or_none(item.get("kejieCount")),
    }


def _int_or_none(value: Any) -> Optional[int]:
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


_ROW_RE = re.compile(r"<tr[^>]*>(.*?)</tr>", re.S | re.I)
_CELL_RE = re.compile(r"<td[^>]*>(.*?)</td>", re.S | re.I)
_CAMPUS_HINTS = ("沙河", "本部", "宏福", "西土城", "校区")
# Observed row (2026-11), 9 cells:
#   0 馆藏部门 | 1 图书条码 | 2 索书号 | 3 登录号 | 4 架位导航 | 5 卷期
#   6 状态 | 7 操作 | 8 空
_STATUS_INDEX = 6
_STATUS_WORDS = ("可借", "借出", "在架", "归还", "预约", "不外借", "遗失", "维修")


def _row_status(cells: list[str]) -> str:
    """Pick the copy status even if the column count shifts across releases."""
    if len(cells) > _STATUS_INDEX and any(
        word in cells[_STATUS_INDEX] for word in _STATUS_WORDS
    ):
        return cells[_STATUS_INDEX]
    # Fall back to any later cell that looks like a status, skipping the
    # 架位导航 / 异地预借 action links.
    for cell in cells[4:]:
        if any(word in cell for word in _STATUS_WORDS):
            return cell
    return ""


def holdings(rec_ctrl_id: str, *, timeout: float = 20.0) -> dict[str, Any]:
    """Parse the holdings table of ``bookInfo_<id>.html``.

    Each row maps to a copy: 馆藏部门 | 图书条码 | 索书号 | 登录号 | ... | 状态.
    Rows are matched by a campus/department cell rather than by fixed column
    index, so layout tweaks do not silently drop data.
    """
    rec_ctrl_id = rec_ctrl_id.strip()
    if not rec_ctrl_id:
        raise LibraryError("recCtrlId 不能为空")
    url = f"{OPAC_BASE}/bookInfo_{rec_ctrl_id}.html"
    with _client(timeout) as client:
        resp = client.get(url, headers={"X-Requested-With": ""})
        if resp.status_code == 404 or resp.status_code >= 500:
            raise LibraryError(
                f"找不到该记录: {rec_ctrl_id}（id 来自 `bupt library search`）"
            )
        resp.raise_for_status()
        page = resp.text

    title = ""
    heading = re.search(r"<title>(.*?)</title>", page, re.S | re.I)
    if heading:
        title = clean_text(heading.group(1))

    copies: list[dict[str, Any]] = []
    for row in _ROW_RE.findall(page):
        cells = [clean_text(cell) for cell in _CELL_RE.findall(row)]
        if not cells:
            continue
        department = cells[0]
        if not any(hint in department for hint in _CAMPUS_HINTS):
            continue
        copies.append(
            {
                "department": department,
                "barcode": cells[1] if len(cells) > 1 else "",
                "call_number": cells[2] if len(cells) > 2 else "",
                "status": _row_status(cells),
            }
        )

    return {
        "id": rec_ctrl_id,
        "url": url,
        "page_title": title,
        "count": len(copies),
        "available": sum(1 for c in copies if "可借" in c["status"]),
        "copies": copies,
    }


def electronic_resources(rec_ctrl_id: str, *, timeout: float = 20.0) -> dict[str, Any]:
    """Read the "其他电子资源" tab of one record.

    ``POST /dianzi.json`` with ``sid=<recCtrlId>`` returns a flat list of
    ``{locate, recNote}`` entries; ``data: []`` means the record has none.
    The location strings sometimes carry a stray \\x1e separator, which is
    stripped. Coverage is heavily skewed to English-language ebooks.
    """
    rec_ctrl_id = rec_ctrl_id.strip()
    if not rec_ctrl_id:
        raise LibraryError("recCtrlId 不能为空")
    url = f"{OPAC_BASE}/dianzi.json"
    with _client(timeout) as client:
        resp = client.post(
            url,
            data={"sid": rec_ctrl_id},
            headers={"Referer": f"{OPAC_BASE}/bookInfo_{rec_ctrl_id}.html"},
        )
        resp.raise_for_status()
        payload = resp.json()

    result = payload.get("result") or {}
    if result.get("code") not in (0, "0"):
        raise LibraryError(f"电子资源查询失败: {result.get('msg') or 'unknown error'}")

    entries: list[dict[str, str]] = []
    for item in payload.get("data") or []:
        if not isinstance(item, dict):
            continue
        locate = str(item.get("locate") or "").replace("\x1e", "").strip()
        note = clean_text(item.get("recNote"))
        if locate or note:
            entries.append({"label": note, "url": locate})

    return {
        "id": rec_ctrl_id,
        "count": len(entries),
        "has_electronic": bool(entries),
        "resources": entries,
    }
