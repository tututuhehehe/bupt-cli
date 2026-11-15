"""portal.py - BUPT information portal notices and news service."""

import json
import re
from typing import Any, Optional, Union
from urllib.parse import urljoin

import httpx
from lxml import etree

from bupt_cli._vendor.campus_api.bupt.auth import AsyncBuptSessionManager, BuptSessionManager
from bupt_cli._vendor.campus_api.bupt.constants import (
    NOTICE_TYPE_ID_MAP,
    NOTICE_TYPE_NAME_MAP,
    NoticeType,
)
from bupt_cli._vendor.campus_api.bupt.models import (
    CampusCardBalance,
    ContentBlock,
    NoticeDetail,
    NoticeItem,
)
from bupt_cli._vendor.campus_api.exceptions import (
    CampusAuthError,
    CampusNetworkError,
    CampusParseError,
    CampusResourceNotFoundError,
)

PORTAL_BASE = "http://my.bupt.edu.cn"
LIST_URL = (
    PORTAL_BASE
    + "/list.jsp?totalpage=30&PAGENUM={page}&urltype=tree.TreeTempUrl&wbtreeid={type_id}"
)
CAMPUS_CARD_BALANCE_URL = f"{PORTAL_BASE}/system/resource/app/cuser/getwxtsA.jsp"
# The widget endpoint rejects requests without this exact referer
# ("错误的访问来源").
CAMPUS_CARD_BALANCE_REFERER = (
    f"{PORTAL_BASE}/xs_index.jsp?urltype=tree.TreeTempUrl&wbtreeid=1541"
)
DEFAULT_NOTICE_TYPES = [NoticeType.NOTICE, NoticeType.NEWS]


def _portal_page_problem(url: str, html_text: str, context: str) -> Optional[Exception]:
    """Classify login and system-error pages that are returned with HTTP 200."""
    lower_url = url.lower()
    lower_html = html_text.lower()

    title_match = re.search(
        r"<title[^>]*>(.*?)</title>", html_text, flags=re.IGNORECASE | re.DOTALL
    )
    title = ""
    if title_match:
        title = re.sub(r"<[^>]+>", " ", title_match.group(1))
        title = re.sub(r"\s+", " ", title).strip().lower()

    login_url = (
        "/system/resource/code/auth/clogin.jsp" in lower_url
        or "authserver/login" in lower_url
    )
    login_html = (
        "/system/resource/code/auth/clogin.jsp" in lower_html
        or "authserver/login" in lower_html
        or re.search(r"name\s*=\s*['\"]execution['\"]", lower_html) is not None
        or (
            re.search(r"(?:name|type)\s*=\s*['\"]password['\"]", lower_html) is not None
            and ("登录" in title or "login" in title)
        )
    )
    if login_url or login_html:
        return CampusAuthError(f"Portal session expired while fetching {context}")

    error_url = "/system/resource/code/error" in lower_url
    error_titles = ("系统提示", "错误提示", "系统错误", "访问出错", "页面错误")
    if error_url or any(marker in title for marker in error_titles):
        return CampusParseError(
            f"Portal returned a system error page while fetching {context}"
        )

    return None


def _parse_portal_html(html_text: str, context: str, url: str = ""):
    """Parse portal HTML and translate invalid documents into SDK exceptions."""
    if not isinstance(html_text, str) or not html_text.strip():
        raise CampusParseError(f"Portal returned empty {context} HTML")

    problem = _portal_page_problem(url, html_text, context)
    if problem is not None:
        raise problem

    try:
        html = etree.HTML(html_text)
    except Exception as e:
        raise CampusParseError(f"Failed to parse portal {context} HTML: {e}") from e
    if html is None:
        raise CampusParseError(f"Failed to parse portal {context} HTML")
    return html


def _response_page_problem(resp: httpx.Response, context: str) -> Optional[Exception]:
    """Return a retryable portal-page problem, if the response represents one."""
    if resp.status_code == 401 or 300 <= resp.status_code < 400:
        return CampusAuthError(f"Portal session expired while fetching {context}")
    if 200 <= resp.status_code < 300:
        return _portal_page_problem(str(resp.url), resp.text, context)
    return None


def _parse_campus_card_balance(text: str) -> CampusCardBalance:
    """Parse the balance widget payload (a small JSON object of strings)."""

    def to_float(value: Any) -> Optional[float]:
        try:
            return float(str(value).strip())
        except (TypeError, ValueError):
            return None

    def to_int(value: Any) -> Optional[int]:
        parsed = to_float(value)
        return int(parsed) if parsed is not None else None

    try:
        payload = json.loads(text)
    except ValueError as e:
        raise CampusParseError(f"Campus card balance returned invalid JSON: {e}") from e
    if not isinstance(payload, dict):
        raise CampusParseError("Campus card balance payload must be a JSON object")

    raw = {
        str(key): "" if value is None else str(value) for key, value in payload.items()
    }
    return CampusCardBalance(
        main_wallet=to_float(raw.get("qkje")),
        subsidy=to_float(raw.get("oddfare")),
        unread_mail=to_int(raw.get("mail")),
        student_id=raw.get("a", ""),
        raw=raw,
    )


def _bytes_page_problem(resp: httpx.Response, context: str) -> Optional[Exception]:
    """Classify HTML login/error bodies smuggled into byte responses."""
    content_type = resp.headers.get("content-type", "").lower()
    if "text/html" not in content_type:
        return None
    return _portal_page_problem(str(resp.url), resp.text, context)


def _is_decoration_image(src: str) -> bool:
    """Heuristically decide whether an image is page chrome, not content."""
    lower_src = src.lower()
    if lower_src.endswith(".gif") and ("arrow" in lower_src or "dot" in lower_src):
        return True
    return any(
        marker in lower_src
        for marker in (
            "topback",
            "drop-shadow",
            "shadow",
            "logo",
            "icon",
            "button",
            "spacer",
            "banner",
            "nav_",
        )
    )


def _extract_blocks(container, base_url: str) -> list[ContentBlock]:
    """Walk a content container and emit paragraph/image blocks in order.

    Text sitting directly around an inline <img> is kept as its own
    paragraph, and relative image srcs resolve against the notice page URL
    (base_url) exactly as a browser would.
    """

    def append_text(raw: str) -> None:
        text = re.sub(r"\s+", " ", raw).strip()
        if text:
            blocks.append(ContentBlock(kind="paragraph", text=text))

    blocks: list[ContentBlock] = []
    append_text(container.text or "")
    for child in container:
        if not isinstance(child.tag, str):
            # Comments / processing instructions: preserve only their tail.
            append_text(child.tail or "")
            continue
        if child.tag.lower() == "img":
            src = (child.get("src") or "").strip()
            if src and not _is_decoration_image(src):
                blocks.append(
                    ContentBlock(
                        kind="image",
                        url=urljoin(base_url, src),
                        alt=(child.get("alt") or "").strip(),
                    )
                )
        elif child.xpath(".//img"):
            blocks.extend(_extract_blocks(child, base_url))
        else:
            append_text("".join(child.itertext()))
        append_text(child.tail or "")
    return blocks


def _validate_notice_query(
    count: int,
    types: Optional[list[Union[NoticeType, str]]],
    page: int,
) -> list[tuple[str, int]]:
    """Validate public query arguments and resolve notice categories."""
    if isinstance(count, bool) or not isinstance(count, int) or count < 0:
        raise ValueError("count must be a non-negative integer")
    if isinstance(page, bool) or not isinstance(page, int) or page < 0:
        raise ValueError("page must be a non-negative integer")
    if types is not None and not isinstance(types, list):
        raise ValueError("types must be a list of NoticeType or string values")

    target_types = DEFAULT_NOTICE_TYPES if types is None else types
    resolved: list[tuple[str, int]] = []
    for notice_type in target_types:
        if isinstance(notice_type, NoticeType):
            type_key = notice_type.value
        elif isinstance(notice_type, str):
            type_key = notice_type.lower().strip()
        else:
            raise ValueError("types must contain only NoticeType or string values")

        type_id = NOTICE_TYPE_ID_MAP.get(type_key)
        if type_id is None:
            raise CampusResourceNotFoundError(f"Unknown notice type '{type_key}'")
        resolved.append((type_key, type_id))

    return resolved


def _extract_author(info_text: str) -> str:
    """Extract a publisher/author without trailing date or metadata fields."""
    marker = re.search(r"(?:发布部门|作者)\s*[:：]", info_text)
    if marker is None:
        return ""

    remainder = info_text[marker.end() :]
    boundaries: list[int] = []
    date_match = re.search(r"\d{4}[-/]\d{2}[-/]\d{2}", remainder)
    if date_match:
        boundaries.append(date_match.start())

    metadata_match = re.search(
        r"(?:发布时间|发布日期|截止日期|截止时间|浏览次数|点击次数|来源|"
        r"责任编辑|编辑|审核|作者|发布部门)\s*[:：]",
        remainder,
    )
    if metadata_match:
        boundaries.append(metadata_match.start())

    if boundaries:
        remainder = remainder[: min(boundaries)]
    return re.sub(r"\s+", " ", remainder).strip(" \t\r\n|｜,，;；")


def _extract_notice_items(html_text: str, type_key: str) -> list[NoticeItem]:
    """Parse list page HTML into NoticeItem objects."""
    html = _parse_portal_html(html_text, "notice list")

    news_lists = html.xpath(
        "//ul[contains(concat(' ', normalize-space(@class), ' '), ' newslist ')]"
    )
    if not news_lists:
        raise CampusParseError(
            "Portal notice list is missing the expected newslist element"
        )

    items = []
    for news_list in news_lists:
        items.extend(news_list.xpath(".//li"))
    if not items:
        return []

    results: list[NoticeItem] = []
    type_name = NOTICE_TYPE_NAME_MAP.get(type_key, type_key)

    for li in items:
        a_tag = li.xpath(".//a")
        if not a_tag:
            continue
        a = a_tag[0]
        href = a.get("href", "").strip()
        title = (a.get("title") or "".join(a.xpath(".//text()"))).strip()
        if not href or not title:
            continue
        full_url = urljoin(PORTAL_BASE + "/", href)

        author_nodes = li.xpath(".//span[contains(@class,'author')]/text()")
        time_nodes = li.xpath(".//span[contains(@class,'time')]/text()")

        author = author_nodes[0].strip() if author_nodes else ""
        time_str = time_nodes[0].strip() if time_nodes else ""

        results.append(
            NoticeItem(
                title=title,
                url=full_url,
                author=author,
                time=time_str,
                type=type_key,
                type_name=type_name,
                raw={"href": href},
            )
        )

    if not results:
        raise CampusParseError("Portal notice list contains no valid notice items")
    return results


def _extract_notice_detail(html_text: str, url: str) -> NoticeDetail:
    """Parse notice detail page HTML into NoticeDetail."""
    html = _parse_portal_html(html_text, "notice detail", url=url)

    # Title
    t_nodes = html.xpath("//h1//text()")
    title = "".join(t.strip() for t in t_nodes if t.strip())
    if not title:
        page_title = "".join(html.xpath("//title/text()")).strip()
        title = page_title.split("-")[0].strip() if page_title else ""
    if not title:
        raise CampusParseError("Portal notice detail is missing a title")

    # Info (Author, Time)
    info_parts = html.xpath(
        "//div[contains(concat(' ', normalize-space(@class), ' '), ' article-info ')]//text() | "
        "//div[contains(concat(' ', normalize-space(@class), ' '), ' info ')]//text()"
    )
    info_text = re.sub(
        r"\s+", " ", " ".join(part.strip() for part in info_parts)
    ).strip()
    time_str = ""
    author_str = _extract_author(info_text)
    time_match = re.search(
        r"(?:发布时间|发布日期)\s*[:：]?\s*(\d{4}[-/]\d{2}[-/]\d{2})",
        info_text,
    ) or re.search(r"(\d{4}[-/]\d{2}[-/]\d{2})", info_text)
    if time_match:
        time_str = time_match.group(1).replace("/", "-")

    # Content
    content_nodes = html.xpath(
        "//div[@id='vsb_content'] | "
        "//div[contains(@class,'v_news_content')] | "
        "//div[contains(@class,'article-content')] | "
        "//div[@class='content']"
    )
    if not content_nodes:
        raise CampusParseError(
            "Portal notice detail is missing the expected content element"
        )
    content_html = etree.tostring(content_nodes[0], encoding="unicode")

    # Attachments
    att_nodes = html.xpath(
        "//a[contains(@href, 'download.jsp') or "
        "contains(@href, 'DownloadAttachUrl') or "
        "contains(@href, 'dfiles')]"
    )
    attachments: list[dict[str, str]] = []
    for a in att_nodes:
        href = a.get("href", "")
        att_title = "".join(a.xpath(".//text()")).strip() or "附件"
        if href and "logout" not in href:
            full_att_url = urljoin(PORTAL_BASE + "/", href)
            attachments.append({"title": att_title, "url": full_att_url})

    # Content blocks (paragraph / image flow). Images resolve relative srcs
    # against the notice page URL; attachments above keep resolving against
    # the site root.
    blocks = _extract_blocks(content_nodes[0], url.strip() or PORTAL_BASE + "/")
    images: list[str] = []
    for block in blocks:
        if block.kind == "image" and block.url not in images:
            images.append(block.url)

    return NoticeDetail(
        title=title,
        url=url,
        author=author_str,
        time=time_str,
        content_html=content_html,
        attachments=attachments,
        images=images,
        blocks=blocks,
        raw_html=html_text,
    )


class PortalService:
    """Synchronous service for querying BUPT information portal notices and news."""

    def __init__(self, session_manager: BuptSessionManager):
        self._session_mgr = session_manager

    def get_campus_card_balance(
        self, retry_on_auth_fail: bool = True
    ) -> CampusCardBalance:
        """Fetch campus card balances from the portal homepage widget.

        Returns:
            CampusCardBalance: 主钱包余额/补助余额 (CNY) plus the raw widget
            payload.
        """
        cli = self._session_mgr.ensure_cas_login()
        try:
            resp = cli.get(
                CAMPUS_CARD_BALANCE_URL,
                headers={
                    "Referer": CAMPUS_CARD_BALANCE_REFERER,
                    "X-Requested-With": "XMLHttpRequest",
                },
            )
            problem = _response_page_problem(resp, "campus card balance")
            if problem is not None:
                if retry_on_auth_fail:
                    self._session_mgr.invalidate()
                    return self.get_campus_card_balance(retry_on_auth_fail=False)
                raise problem

            resp.raise_for_status()
            return _parse_campus_card_balance(resp.text)
        except (CampusAuthError, CampusParseError):
            raise
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"Campus card balance fetch failed: {e}") from e

    def get_notices(
        self,
        count: int = 10,
        types: Optional[list[Union[NoticeType, str]]] = None,
        page: int = 0,
        retry_on_auth_fail: bool = True,
    ) -> list[NoticeItem]:
        """Fetch latest portal notifications and news.

        Args:
            count (int): Maximum total items to return (default: 10).
            types (Optional[list]): List of notice types (default: [NoticeType.NOTICE, NoticeType.NEWS]).
            page (int): Page index (0-indexed, default: 0).
            retry_on_auth_fail (bool): Whether to auto re-login on session failure.

        Returns:
            list[NoticeItem]: Cleaned notice items sorted by time descending.
        """
        target_types = _validate_notice_query(count, types, page)
        if count == 0 or not target_types:
            return []

        cli = self._session_mgr.ensure_cas_login()
        all_items: list[NoticeItem] = []

        for type_key, type_id in target_types:
            url = LIST_URL.format(page=page, type_id=type_id)
            try:
                resp = cli.get(url)
                problem = _response_page_problem(resp, "notice list")
                if problem is not None:
                    if retry_on_auth_fail:
                        self._session_mgr.invalidate()
                        return self.get_notices(
                            count=count,
                            types=types,
                            page=page,
                            retry_on_auth_fail=False,
                        )
                    raise problem

                resp.raise_for_status()
                items = _extract_notice_items(resp.text, type_key=type_key)
                all_items.extend(items)
            except (CampusAuthError, CampusParseError):
                raise
            except httpx.HTTPError as e:
                raise CampusNetworkError(
                    f"Portal notice fetch failed for type '{type_key}': {e}"
                ) from e

        # Sort by date descending
        all_items.sort(key=lambda x: x.time or "0000-00-00", reverse=True)
        return all_items[:count]

    def get_notice_detail(
        self, url_or_item: Union[str, NoticeItem], retry_on_auth_fail: bool = True
    ) -> NoticeDetail:
        """Fetch detailed content and attachment links of a notice."""
        url = (
            url_or_item.url
            if isinstance(url_or_item, NoticeItem)
            else str(url_or_item).strip()
        )
        if not url:
            raise ValueError("notice URL must not be empty")
        cli = self._session_mgr.ensure_cas_login()

        try:
            resp = cli.get(url)
            problem = _response_page_problem(resp, "notice detail")
            if problem is not None:
                if retry_on_auth_fail:
                    self._session_mgr.invalidate()
                    return self.get_notice_detail(url_or_item, retry_on_auth_fail=False)
                raise problem

            resp.raise_for_status()
            return _extract_notice_detail(resp.text, url=url)
        except (CampusAuthError, CampusParseError):
            raise
        except httpx.HTTPError as e:
            raise CampusNetworkError(
                f"Failed to fetch notice detail from '{url}': {e}"
            ) from e


class AsyncPortalService:
    """Asynchronous service for querying BUPT information portal notices and news."""

    def __init__(self, session_manager: AsyncBuptSessionManager):
        self._session_mgr = session_manager

    async def get_campus_card_balance(
        self, retry_on_auth_fail: bool = True
    ) -> CampusCardBalance:
        """Fetch campus card balances from the portal homepage widget asynchronously."""
        cli = await self._session_mgr.ensure_cas_login()
        try:
            resp = await cli.get(
                CAMPUS_CARD_BALANCE_URL,
                headers={
                    "Referer": CAMPUS_CARD_BALANCE_REFERER,
                    "X-Requested-With": "XMLHttpRequest",
                },
            )
            problem = _response_page_problem(resp, "campus card balance")
            if problem is not None:
                if retry_on_auth_fail:
                    self._session_mgr.invalidate()
                    return await self.get_campus_card_balance(retry_on_auth_fail=False)
                raise problem

            resp.raise_for_status()
            return _parse_campus_card_balance(resp.text)
        except (CampusAuthError, CampusParseError):
            raise
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"Campus card balance fetch failed: {e}") from e

    async def get_notices(
        self,
        count: int = 10,
        types: Optional[list[Union[NoticeType, str]]] = None,
        page: int = 0,
        retry_on_auth_fail: bool = True,
    ) -> list[NoticeItem]:
        """Fetch latest portal notifications and news asynchronously."""
        target_types = _validate_notice_query(count, types, page)
        if count == 0 or not target_types:
            return []

        cli = await self._session_mgr.ensure_cas_login()
        all_items: list[NoticeItem] = []

        for type_key, type_id in target_types:
            url = LIST_URL.format(page=page, type_id=type_id)
            try:
                resp = await cli.get(url)
                problem = _response_page_problem(resp, "notice list")
                if problem is not None:
                    if retry_on_auth_fail:
                        self._session_mgr.invalidate()
                        return await self.get_notices(
                            count=count,
                            types=types,
                            page=page,
                            retry_on_auth_fail=False,
                        )
                    raise problem

                resp.raise_for_status()
                items = _extract_notice_items(resp.text, type_key=type_key)
                all_items.extend(items)
            except (CampusAuthError, CampusParseError):
                raise
            except httpx.HTTPError as e:
                raise CampusNetworkError(
                    f"Portal notice fetch failed for type '{type_key}': {e}"
                ) from e

        all_items.sort(key=lambda x: x.time or "0000-00-00", reverse=True)
        return all_items[:count]

    async def get_notice_detail(
        self, url_or_item: Union[str, NoticeItem], retry_on_auth_fail: bool = True
    ) -> NoticeDetail:
        """Fetch detailed content and attachment links of a notice asynchronously."""
        url = (
            url_or_item.url
            if isinstance(url_or_item, NoticeItem)
            else str(url_or_item).strip()
        )
        if not url:
            raise ValueError("notice URL must not be empty")
        cli = await self._session_mgr.ensure_cas_login()

        try:
            resp = await cli.get(url)
            problem = _response_page_problem(resp, "notice detail")
            if problem is not None:
                if retry_on_auth_fail:
                    self._session_mgr.invalidate()
                    return await self.get_notice_detail(
                        url_or_item, retry_on_auth_fail=False
                    )
                raise problem

            resp.raise_for_status()
            return _extract_notice_detail(resp.text, url=url)
        except (CampusAuthError, CampusParseError):
            raise
        except httpx.HTTPError as e:
            raise CampusNetworkError(
                f"Failed to fetch notice detail from '{url}': {e}"
            ) from e
