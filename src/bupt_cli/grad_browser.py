"""Headless browser collector for the graduate system (pure CDP).

yjxt.bupt.edu.cn sits behind Riversafe (瑞数), which answers a plain HTTP
request with an obfuscated challenge script. A real browser satisfies it by
running the script, so the CLI drives a headless Chrome over the DevTools
Protocol and reads the rendered DOM.

Two findings that make a *headless* run work at all (verified 2026-11):

1. The default headless user-agent contains ``HeadlessChrome``; the CAS page
   then refuses to paint its login iframe (frame exists, document is empty).
   Passing a normal desktop UA fixes it.
2. The CAS form lives inside an iframe, so clicks must be dispatched from
   inside that frame's document rather than to the top-level page.

No agent-browser / Playwright / Selenium: :mod:`bupt_cli.cdp` speaks the
protocol directly.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.request
from pathlib import Path
from typing import Any, Optional

CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
# A normal desktop UA. Without it CAS sees "HeadlessChrome" and renders an
# empty iframe.
USER_AGENT = (
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36"
)
LOGIN_URL = (
    "https://auth.bupt.edu.cn/authserver/login"
    "?service=http%3a%2f%2fyjxt.bupt.edu.cn%2fULogin.aspx"
)
YJXT_BASE = "https://yjxt.bupt.edu.cn"
MENU_URL = f"{YJXT_BASE}/Gstudent/LeftMenu.aspx?menu=00"

_MENU_RE = re.compile(r"name:\s*'([^']+)',\s*t:\s*'[^']*',\s*url:\s*'([^']+)'")
_CAL_EID_RE = re.compile(r"TermCalender\.aspx\?EID=([^&'\"]+)")


class BrowserError(Exception):
    """The driving browser could not complete the flow."""

    def __init__(self, message: str, hint: str = "") -> None:
        super().__init__(message)
        self.hint = hint


def find_chrome() -> Optional[str]:
    """Locate a usable Chrome/Chromium binary, or return None."""
    candidates = [
        CHROME,
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        os.path.expanduser("~/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
    ]
    for path in candidates:
        if os.path.exists(path):
            return path
    return shutil.which("google-chrome") or shutil.which("chromium")


CHROME_MISSING_HINT = (
    "研究生系统需要真实浏览器（瑞数防护无法用纯 HTTP 绕过）。"
    "请安装 Google Chrome：https://www.google.com/chrome/"
)


def _free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def _wait_devtools(port: int, timeout: float = 20.0) -> dict[str, Any]:
    """Poll /json/version until Chrome's debug endpoint answers."""
    deadline = time.time() + timeout
    url = f"http://127.0.0.1:{port}/json/version"
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=1) as resp:
                return json.load(resp)
        except Exception:
            time.sleep(0.1)
    raise BrowserError("Chrome 调试端口没有起来", hint="确认已安装 Google Chrome")


def _first_page(port: int, timeout: float = 20.0) -> dict[str, Any]:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(
                f"http://127.0.0.1:{port}/json/list", timeout=1
            ) as resp:
                tabs = json.load(resp)
            pages = [t for t in tabs if t.get("type") == "page"]
            if pages:
                return pages[0]
        except Exception:
            pass
        time.sleep(0.1)
    raise BrowserError("Chrome 没有可用的页面 target")


class Driver:
    """Owns one headless Chrome and one page target."""

    def __init__(self, *, headless: bool = True) -> None:
        chrome = find_chrome()
        if not chrome:
            raise BrowserError("没有找到 Google Chrome", hint=CHROME_MISSING_HINT)
        self.port = _free_port()
        self.profile = Path(tempfile.mkdtemp(prefix="bupt-grad-"))
        args = [
            chrome,
            f"--remote-debugging-port={self.port}",
            f"--user-data-dir={self.profile}",
            f"--user-agent={USER_AGENT}",
            "--window-size=1512,982",
            "--disable-blink-features=AutomationControlled",
            "--disable-extensions",
            "--no-first-run",
            "--no-default-browser-check",
        ]
        if headless:
            args.append("--headless=new")
        args.append("about:blank")
        self._proc = subprocess.Popen(
            args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )
        self._cdp: Any = None
        try:
            _wait_devtools(self.port)
            from bupt_cli.cdp import CDP

            page = _first_page(self.port)
            self._cdp = CDP(page["webSocketDebuggerUrl"])
            self._cdp.call("Page.enable")
            self._cdp.call("Runtime.enable")
            self._cdp.call(
                "Network.setUserAgentOverride",
                {"userAgent": USER_AGENT, "platform": "MacIntel"},
            )
        except Exception:
            self.close()
            raise

    # -- page helpers ------------------------------------------------------
    def goto(self, url: str, *, settle: float = 0.0, timeout: float = 30.0) -> None:
        self._cdp.call("Page.navigate", {"url": url}, timeout=timeout)
        deadline = time.time() + timeout
        while time.time() < deadline:
            state = self._cdp.eval("document.readyState")
            if state == "complete":
                break
            time.sleep(0.05)
        if settle:
            time.sleep(settle)

    def url(self) -> str:
        return str(self._cdp.eval("location.href") or "")

    def eval(self, js: str, *, timeout: float = 30.0) -> Any:
        return self._cdp.eval(js, timeout=timeout)

    def frame_js(self, body: str) -> Any:
        """Run ``body`` inside the first iframe's document.

        ``body`` is a JS function body that may ``return`` a value. Doing this
        through ``contentDocument`` avoids the "click landed on the top page"
        problem the CAS layout creates.
        """
        script = (
            "(()=>{const f=document.querySelector('iframe');"
            "if(!f||!f.contentDocument)return '__NO_FRAME__';"
            f"const d=f.contentDocument;{body}}})()"
        )
        return self._cdp.eval(script)

    def wait_for(self, predicate, *, timeout: float = 30.0, interval: float = 0.08):
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                value = predicate()
            except Exception:
                value = None
            if value:
                return value
            time.sleep(interval)
        return None

    def close(self) -> None:
        try:
            self._cdp.close()
        except Exception:
            pass
        try:
            self._proc.terminate()
        except Exception:
            pass
        subprocess.run(
            ["pkill", "-f", f"remote-debugging-port={self.port}"], check=False
        )
        subprocess.run(["pkill", "-f", str(self.profile)], check=False)
        for _ in range(10):
            shutil.rmtree(self.profile, ignore_errors=True)
            if not self.profile.exists():
                break
            time.sleep(0.1)

    def __enter__(self) -> "Driver":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()


# ---------------------------------------------------------------------------
# Graduate-system flow
# ---------------------------------------------------------------------------

_FILL_JS = (
    "const u=d.querySelector('#username'),p=d.querySelector('#password');"
    "if(!u||!p)return 'no-inputs';"
    "u.value={user};p.value={secret};"
    "u.dispatchEvent(new Event('input',{{bubbles:true}}));"
    "p.dispatchEvent(new Event('input',{{bubbles:true}}));"
    "return 'filled';"
)

_SUBMIT_JS = (
    "const b=[...d.querySelectorAll('input[type=button],button')]"
    ".find(x=>(x.value||x.textContent||'').trim()==='账号登录');"
    "if(b){b.click();return 'clicked'}return 'none';"
)


def login(driver: Driver, username: str, password: str) -> None:
    driver.goto(LOGIN_URL)
    # Wait for the CAS iframe's DOM before touching it: clicking too early is a
    # no-op and the tab never switches.
    if driver.wait_for(
        lambda: driver.frame_js(
            "return d.querySelector('a')&&[...d.querySelectorAll('a')]"
            ".some(x=>x.textContent.trim()==='密码登录')?1:0;"
        ),
        timeout=20,
    ) is None:
        raise BrowserError("CAS 登录页的 iframe 没有加载出登录标签")

    # The CAS page ships #username hidden behind the QR panel. Wait for it to
    # become visible; if it never does, click the 密码登录 tab once and wait
    # again. Checking `class="active"` is unreliable during initialisation, and
    # clicking an already-active tab toggles it back to QR mode.
    visible_js = (
        "const u=d.querySelector('#username');"
        "return u&&u.offsetParent!==null?1:0;"
    )
    if driver.wait_for(lambda: driver.frame_js(visible_js), timeout=1.2) is None:
        driver.frame_js(
            "const a=[...d.querySelectorAll('a')]"
            ".find(x=>x.textContent.trim()==='密码登录');if(a)a.click();return 1;"
        )
        if driver.wait_for(lambda: driver.frame_js(visible_js), timeout=12) is None:
            raise BrowserError("切换到密码登录失败", hint="重试一次")

    result = driver.frame_js(
        _FILL_JS.format(
            user=json.dumps(username, ensure_ascii=False),
            secret=json.dumps(password, ensure_ascii=False),
        )
    )
    if result != "filled":
        raise BrowserError(f"填写账号密码失败：{result}")

    # The page wires up its submit handler after the tab switch; wait for the
    # button to exist, then click exactly once (a retried click can double-submit).
    if driver.wait_for(
        lambda: driver.frame_js(
            "const b=[...d.querySelectorAll('input[type=button],button')]"
            ".find(x=>(x.value||x.textContent||'').trim()==='账号登录');"
            "return b?1:0;"
        ),
        timeout=10,
    ) is None:
        raise BrowserError("找不到「账号登录」按钮")
    if driver.frame_js(_SUBMIT_JS) != "clicked":
        raise BrowserError("点击「账号登录」失败")

    if driver.wait_for(
        lambda: "yjxt.bupt.edu.cn" in driver.url(), timeout=30
    ) is None:
        raise BrowserError(
            "登录没有进入研究生系统",
            hint="检查 credentials.json 里的学号/密码是否正确",
        )

    # Let the redirect chain settle before the next navigation.
    previous = ""
    for _ in range(20):
        current = driver.url()
        if current == previous:
            break
        previous = current
        time.sleep(0.08)


def _menu(driver: Driver) -> dict[str, str]:
    driver.goto(MENU_URL)
    # One round-trip per poll: innerText is empty on this page, so read the
    # HTML once instead of trying both.
    entries = driver.wait_for(
        lambda: _MENU_RE.findall(
            driver.eval("document.documentElement.outerHTML") or ""
        ),
        timeout=20,
    )
    if not entries:
        raise BrowserError("菜单页没有返回内容", hint="重新执行本命令")
    return {name: f"{YJXT_BASE}/Gstudent/{url}" for name, url in entries}


def collect(
    username: str,
    password: str,
    *,
    want: tuple[str, ...] = ("schedule", "calendar"),
    headless: bool = True,
) -> dict[str, Any]:
    """Log in once, then return the HTML of each requested page."""
    pages: dict[str, Any] = {}
    with Driver(headless=headless) as driver:
        login(driver, username, password)
        menu = _menu(driver)

        def page_url(name: str) -> str:
            for key, url in menu.items():
                if name in key:
                    return url
            raise BrowserError(f"菜单里找不到「{name}」")

        if "schedule" in want:
            # Riversafe occasionally serves its challenge page instead of the
            # timetable; a reload clears it, so retry a couple of times.
            html = None
            for attempt in range(3):
                driver.goto(page_url("学期课表信息查询"))
                html = driver.wait_for(
                    lambda: (
                        lambda h: h if "星期一" in (h or "") else None
                    )(driver.eval("document.documentElement.outerHTML")),
                    timeout=12,
                )
                if html:
                    break
            if not html:
                raise BrowserError("课表页没有渲染出表格", hint="重试一次")
            pages["schedule"] = html

        if "calendar" in want:
            source = pages.get("schedule", "")
            match = _CAL_EID_RE.search(source or "")
            if not match:
                driver.goto(page_url("学期课表信息查询"))
                match = _CAL_EID_RE.search(
                    driver.eval("document.documentElement.outerHTML") or ""
                )
            if not match:
                raise BrowserError("课表页里找不到教学日历入口")
            driver.goto(
                f"{YJXT_BASE}/PublicPage/TermCalender.aspx?EID={match.group(1)}&UID=",
            )
            # Wait until the table is fully painted: Riversafe injects the rows
            # progressively, so a half-rendered table silently loses weeks.
            cal = driver.wait_for(
                lambda: (
                    lambda h: h
                    if h
                    and "dgData" in h
                    and "二十" in h
                    else None
                )(driver.eval("document.documentElement.outerHTML")),
                timeout=25,
                interval=0.4,
            )
            if not cal:
                raise BrowserError("教学日历页面没有渲染出来")
            pages["calendar"] = cal

        if "exams" in want:
            # Retry like the timetable: a Riversafe challenge must not be
            # reported as "no exams scheduled".
            exams = None
            for _attempt in range(3):
                driver.goto(page_url("学期考核信息查询"))
                exams = driver.wait_for(
                    lambda: (
                        lambda h: h if "考试课程" in (h or "") else None
                    )(driver.eval("document.documentElement.outerHTML")),
                    timeout=12,
                )
                if exams:
                    break
            if not exams:
                raise BrowserError("考试安排页没有渲染出来", hint="重试一次")
            pages["exams"] = exams

    return pages
