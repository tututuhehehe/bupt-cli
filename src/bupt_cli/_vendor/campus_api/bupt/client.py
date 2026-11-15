"""client.py - Unified BuptClient and AsyncBuptClient facades."""

import datetime as dt
import logging
from typing import Any, Optional, Union

import httpx

from bupt_cli._vendor.campus_api.bupt.auth import (
    AsyncBuptSessionManager,
    AsyncJwglSessionManager,
    BuptSessionManager,
    JwglSessionManager,
)
from bupt_cli._vendor.campus_api.bupt.constants import BuptBuilding, CampusArea, NoticeType
from bupt_cli._vendor.campus_api.bupt.electricity import AsyncElectricityService, ElectricityService
from bupt_cli._vendor.campus_api.bupt.homework import AsyncHomeworkService, HomeworkService
from bupt_cli._vendor.campus_api.bupt.mobile_jwgl import (
    AsyncMobileJwglService,
    AsyncMobileJwglSessionManager,
    MobileJwglService,
    MobileJwglSessionManager,
    SjdClassroomEntry,
    SjdCurriculumWeek,
    SjdKeyedValue,
    SjdKbMode,
    SjdScoreReport,
    curriculum_to_overview,
)
from bupt_cli._vendor.campus_api.bupt.models import (
    CampusCardBalance,
    CourseItem,
    CourseSite,
    ElectricityInfo,
    HomeworkDetail,
    HomeworkItem,
    HomeworkOverview,
    NoticeDetail,
    NoticeItem,
    ScheduleOverview,
)
from bupt_cli._vendor.campus_api.bupt.portal import (
    AsyncPortalService,
    PortalService,
    _bytes_page_problem,
)
from bupt_cli._vendor.campus_api.bupt.schedule import AsyncScheduleService, ScheduleService
from bupt_cli._vendor.campus_api.exceptions import (
    CampusAuthError,
    CampusError,
    CampusNetworkError,
    CampusParseError,
)

logger = logging.getLogger(__name__)


class BuptClient:
    """Unified synchronous client for accessing BUPT campus services.

    Usage:
    ```python
    from bupt_cli._vendor.campus_api import BuptClient
    from bupt_cli._vendor.campus_api.bupt import BuptBuilding, NoticeType

    with BuptClient(username="...", password="...", jwgl_password="...") as client:
        # Electricity
        elec = client.get_electricity(building=BuptBuilding.SHAHE_A, room=101)

        # Homework
        undone = client.get_undone_homework()

        # Schedule
        sched = client.get_schedule(week=3)

        # Portal notices
        notices = client.get_notices(count=5)
    ```
    """

    def __init__(
        self,
        username: str = "",
        password: str = "",
        jwgl_username: Optional[str] = None,
        jwgl_password: Optional[str] = None,
        timeout: float = 15.0,
    ):
        self.username = username
        self.password = password
        self.jwgl_username = jwgl_username or username
        self.jwgl_password = jwgl_password or password
        self.timeout = timeout

        self._cas_session_mgr = BuptSessionManager(
            username=username, password=password, timeout=timeout
        )
        self._jwgl_session_mgr = JwglSessionManager(
            username=self.jwgl_username,
            password=self.jwgl_password,
            timeout=timeout,
        )
        self._mobile_jwgl_session_mgr = MobileJwglSessionManager(
            username=self.jwgl_username,
            password=self.jwgl_password,
            timeout=timeout,
        )

        self._electricity_service: Optional[ElectricityService] = None
        self._homework_service: Optional[HomeworkService] = None
        self._schedule_service: Optional[ScheduleService] = None
        self._portal_service: Optional[PortalService] = None
        self._mobile_jwgl_service: Optional[MobileJwglService] = None

    @property
    def electricity(self) -> ElectricityService:
        """Access the underlying ElectricityService."""
        if self._electricity_service is None:
            self._electricity_service = ElectricityService(self._cas_session_mgr)
        return self._electricity_service

    @property
    def homework(self) -> HomeworkService:
        """Access the underlying HomeworkService."""
        if self._homework_service is None:
            self._homework_service = HomeworkService(self._cas_session_mgr)
        return self._homework_service

    @property
    def schedule(self) -> ScheduleService:
        """Access the underlying ScheduleService."""
        if self._schedule_service is None:
            self._schedule_service = ScheduleService(self._jwgl_session_mgr)
        return self._schedule_service

    @property
    def mobile_jwgl(self) -> MobileJwglService:
        """Access the underlying MobileJwglService."""
        if self._mobile_jwgl_service is None:
            self._mobile_jwgl_service = MobileJwglService(self._mobile_jwgl_session_mgr)
        return self._mobile_jwgl_service

    @property
    def portal(self) -> PortalService:
        """Access the underlying PortalService."""
        if self._portal_service is None:
            self._portal_service = PortalService(self._cas_session_mgr)
        return self._portal_service

    def get_electricity(
        self,
        building: BuptBuilding,
        room: Union[int, str],
        floor: Optional[Union[int, str]] = None,
    ) -> ElectricityInfo:
        """Query dormitory electricity balance."""
        return self.electricity.query(building=building, room=room, floor=floor)

    def get_electricity_buildings(
        self,
        area: CampusArea = CampusArea.SHAHE,
    ) -> list[dict[str, Any]]:
        """Get all registered dorm buildings for a campus area."""
        return self.electricity.get_buildings(area=area)

    def get_undone_homework(self) -> HomeworkOverview:
        """Fetch pending (unfinished) assignments across all enrolled courses."""
        return self.homework.get_undone()

    def get_all_homework(
        self, site_limit: int = 30, page_size: int = 50
    ) -> list[HomeworkItem]:
        """Fetch all homework assignments across all enrolled courses."""
        return self.homework.get_all(site_limit=site_limit, page_size=page_size)

    def get_homework_detail(self, activity_id: str) -> HomeworkDetail:
        """Fetch detailed information for a specific homework assignment."""
        return self.homework.get_detail(activity_id=activity_id)

    def get_enrolled_courses(self, size: int = 50, page: int = 1) -> list[CourseSite]:
        """Fetch currently enrolled course sites."""
        return self.homework.get_courses(size=size, page=page)

    def get_schedule(
        self,
        week: Optional[int] = None,
        source: Optional[str] = None,
    ) -> ScheduleOverview:
        """Fetch course schedule for a given academic week (1-25).

        The mobile JSON backend serves the current week natively and is
        tried first; other weeks (or any mobile failure) fall back to the
        DOM parser.

        Args:
            week (Optional[int]): Week number. Computed from the semester
                anchor if omitted.
            source (Optional[str]): Force a backend: "mobile" or "dom".
                None auto-selects. "mobile" raises instead of falling back.
        """
        if source not in (None, "mobile", "dom"):
            raise ValueError("source must be None, 'mobile' or 'dom'")
        if source != "dom":
            try:
                overview = curriculum_to_overview(self.mobile_jwgl.get_curriculum())
                if week is None or overview.week == week:
                    return overview
                if source == "mobile":
                    raise CampusParseError(
                        "mobile curriculum only serves the current week "
                        f"(week={overview.week}, requested week={week})"
                    )
            except CampusError as error:
                if source == "mobile":
                    raise
                logger.warning(
                    "mobile schedule unavailable (%s); falling back to DOM", error
                )
        return self.schedule.get_schedule(week=week)

    def get_today_schedule(
        self,
        week: Optional[int] = None,
        target_date: Optional[dt.date] = None,
        source: Optional[str] = None,
    ) -> list[CourseItem]:
        """Fetch courses scheduled for today (or specified date).

        Uses the mobile backend when the query is about the current week
        (no explicit week, and target date is today or omitted); otherwise
        falls back to the DOM parser.
        """
        if source not in (None, "mobile", "dom"):
            raise ValueError("source must be None, 'mobile' or 'dom'")
        mobile_applies = week is None and (
            target_date is None or target_date == dt.date.today()
        )
        if source == "mobile" or (source is None and mobile_applies):
            try:
                overview = curriculum_to_overview(self.mobile_jwgl.get_curriculum())
                return overview.get_today_courses(target_date=target_date)
            except CampusError as error:
                if source == "mobile":
                    raise
                logger.warning(
                    "mobile schedule unavailable (%s); falling back to DOM", error
                )
        return self.schedule.get_today_schedule(week=week, target_date=target_date)

    def get_curriculum(self) -> list[SjdCurriculumWeek]:
        """Fetch the raw current-week curriculum grid from the mobile backend."""
        return self.mobile_jwgl.get_curriculum()

    def get_schedule_modes(self) -> list[SjdKbMode]:
        """Fetch available schedule time modes (节次模式) from the mobile backend."""
        return self.mobile_jwgl.get_schedule_modes()

    def get_exam_weeks(self) -> list[SjdKeyedValue]:
        """Fetch exam week options (考试周次) from the mobile backend."""
        return self.mobile_jwgl.get_exam_weeks()

    def get_exam_schedule(self) -> list[dict[str, Any]]:
        """Fetch the exam schedule (raw dicts; empty between semesters)."""
        return self.mobile_jwgl.get_exam_schedule()

    def get_exam_course_schedule(self) -> list[dict[str, Any]]:
        """Fetch exam course arrangements (raw dicts; empty between semesters)."""
        return self.mobile_jwgl.get_exam_course_schedule()

    def get_jwgl_notices(self) -> list[dict[str, Any]]:
        """Fetch JWGL notices (raw dicts). Portal notices: see get_notices."""
        return self.mobile_jwgl.get_jwgl_notices()

    def get_score_report(self) -> SjdScoreReport:
        """Fetch the mobile JWGL transcript report (termGPA).

        Covers all recorded semesters of per-course grades plus the GPA
        summary; filter with :meth:`SjdScoreReport.courses_in_semester`.
        """
        return self.mobile_jwgl.get_score_report()

    def get_today_classrooms(
        self, campus_id: Optional[str] = None
    ) -> list[SjdClassroomEntry]:
        """Fetch real-time empty classrooms from the mobile backend.

        Args:
            campus_id (Optional[str]): "01" 西土城路校区, "04" 沙河校区.
        """
        return self.mobile_jwgl.get_today_classrooms(campus_id=campus_id)

    def get_notices(
        self,
        count: int = 10,
        types: Optional[list[Union[NoticeType, str]]] = None,
        page: int = 0,
    ) -> list[NoticeItem]:
        """Fetch latest portal notifications and news."""
        return self.portal.get_notices(count=count, types=types, page=page)

    def get_notice_detail(self, url_or_item: Union[str, NoticeItem]) -> NoticeDetail:
        """Fetch detailed content and attachments of a portal notice."""
        return self.portal.get_notice_detail(url_or_item=url_or_item)

    def get_campus_card_balance(self) -> CampusCardBalance:
        """Fetch campus card balances (主钱包/补助) from the portal homepage."""
        return self.portal.get_campus_card_balance()

    def get_bytes(self, url: str, retry_on_auth_fail: bool = True) -> bytes:
        """Fetch raw bytes (e.g. an image) via the authenticated CAS session.

        HTTP errors and login/error pages returned with HTTP 200 are rejected
        (auth problems retry once with a fresh session), so callers never
        receive auth HTML as if it were the requested resource.
        """
        if not isinstance(url, str) or not url.strip():
            raise ValueError("URL must not be empty")
        cli = self._cas_session_mgr.ensure_cas_login()
        try:
            resp = cli.get(url)
            problem: Optional[Exception] = None
            if resp.status_code == 401 or 300 <= resp.status_code < 400:
                problem = CampusAuthError(
                    f"Portal session expired while fetching bytes from '{url}'"
                )
            elif 200 <= resp.status_code < 300:
                problem = _bytes_page_problem(resp, f"bytes fetch from '{url}'")
            if problem is not None:
                if retry_on_auth_fail and isinstance(problem, CampusAuthError):
                    self._cas_session_mgr.invalidate()
                    return self.get_bytes(url, retry_on_auth_fail=False)
                raise problem

            resp.raise_for_status()
            return resp.content
        except (CampusAuthError, CampusParseError):
            raise
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"Failed to fetch bytes from '{url}': {e}") from e

    def close(self):
        """Close active network sessions."""
        self._cas_session_mgr.close()
        self._jwgl_session_mgr.close()
        self._mobile_jwgl_session_mgr.close()

    def __enter__(self) -> "BuptClient":
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.close()


class AsyncBuptClient:
    """Unified asynchronous client for accessing BUPT campus services.

    Usage:
    ```python
    import asyncio
    from bupt_cli._vendor.campus_api import AsyncBuptClient
    from bupt_cli._vendor.campus_api.bupt import BuptBuilding

    async def main():
        async with AsyncBuptClient(username="...", password="...", jwgl_password="...") as client:
            elec = await client.get_electricity(building=BuptBuilding.SHAHE_A, room=101)
            undone = await client.get_undone_homework()
            sched = await client.get_schedule(week=3)
            notices = await client.get_notices(count=5)

    asyncio.run(main())
    ```
    """

    def __init__(
        self,
        username: str = "",
        password: str = "",
        jwgl_username: Optional[str] = None,
        jwgl_password: Optional[str] = None,
        timeout: float = 15.0,
    ):
        self.username = username
        self.password = password
        self.jwgl_username = jwgl_username or username
        self.jwgl_password = jwgl_password or password
        self.timeout = timeout

        self._cas_session_mgr = AsyncBuptSessionManager(
            username=username, password=password, timeout=timeout
        )
        self._jwgl_session_mgr = AsyncJwglSessionManager(
            username=self.jwgl_username,
            password=self.jwgl_password,
            timeout=timeout,
        )
        self._mobile_jwgl_session_mgr = AsyncMobileJwglSessionManager(
            username=self.jwgl_username,
            password=self.jwgl_password,
            timeout=timeout,
        )

        self._electricity_service: Optional[AsyncElectricityService] = None
        self._homework_service: Optional[AsyncHomeworkService] = None
        self._schedule_service: Optional[AsyncScheduleService] = None
        self._portal_service: Optional[AsyncPortalService] = None
        self._mobile_jwgl_service: Optional[AsyncMobileJwglService] = None

    @property
    def electricity(self) -> AsyncElectricityService:
        """Access the underlying AsyncElectricityService."""
        if self._electricity_service is None:
            self._electricity_service = AsyncElectricityService(self._cas_session_mgr)
        return self._electricity_service

    @property
    def homework(self) -> AsyncHomeworkService:
        """Access the underlying AsyncHomeworkService."""
        if self._homework_service is None:
            self._homework_service = AsyncHomeworkService(self._cas_session_mgr)
        return self._homework_service

    @property
    def schedule(self) -> AsyncScheduleService:
        """Access the underlying AsyncScheduleService."""
        if self._schedule_service is None:
            self._schedule_service = AsyncScheduleService(self._jwgl_session_mgr)
        return self._schedule_service

    @property
    def mobile_jwgl(self) -> AsyncMobileJwglService:
        """Access the underlying AsyncMobileJwglService."""
        if self._mobile_jwgl_service is None:
            self._mobile_jwgl_service = AsyncMobileJwglService(
                self._mobile_jwgl_session_mgr
            )
        return self._mobile_jwgl_service

    @property
    def portal(self) -> AsyncPortalService:
        """Access the underlying AsyncPortalService."""
        if self._portal_service is None:
            self._portal_service = AsyncPortalService(self._cas_session_mgr)
        return self._portal_service

    async def get_electricity(
        self,
        building: BuptBuilding,
        room: Union[int, str],
        floor: Optional[Union[int, str]] = None,
    ) -> ElectricityInfo:
        """Query dormitory electricity balance asynchronously."""
        return await self.electricity.query(building=building, room=room, floor=floor)

    async def get_electricity_buildings(
        self,
        area: CampusArea = CampusArea.SHAHE,
    ) -> list[dict[str, Any]]:
        """Get all registered dorm buildings for a campus area asynchronously."""
        return await self.electricity.get_buildings(area=area)

    async def get_undone_homework(self) -> HomeworkOverview:
        """Fetch pending (unfinished) assignments across all enrolled courses asynchronously."""
        return await self.homework.get_undone()

    async def get_all_homework(
        self, site_limit: int = 30, page_size: int = 50
    ) -> list[HomeworkItem]:
        """Fetch all homework assignments across all enrolled courses asynchronously."""
        return await self.homework.get_all(site_limit=site_limit, page_size=page_size)

    async def get_homework_detail(self, activity_id: str) -> HomeworkDetail:
        """Fetch detailed information for a specific homework assignment asynchronously."""
        return await self.homework.get_detail(activity_id=activity_id)

    async def get_enrolled_courses(
        self, size: int = 50, page: int = 1
    ) -> list[CourseSite]:
        """Fetch currently enrolled course sites asynchronously."""
        return await self.homework.get_courses(size=size, page=page)

    async def get_schedule(
        self,
        week: Optional[int] = None,
        source: Optional[str] = None,
    ) -> ScheduleOverview:
        """Fetch course schedule for a given academic week asynchronously.

        The mobile JSON backend serves the current week natively and is
        tried first; other weeks (or any mobile failure) fall back to the
        DOM parser.
        """
        if source not in (None, "mobile", "dom"):
            raise ValueError("source must be None, 'mobile' or 'dom'")
        if source != "dom":
            try:
                overview = curriculum_to_overview(
                    await self.mobile_jwgl.get_curriculum()
                )
                if week is None or overview.week == week:
                    return overview
                if source == "mobile":
                    raise CampusParseError(
                        "mobile curriculum only serves the current week "
                        f"(week={overview.week}, requested week={week})"
                    )
            except CampusError as error:
                if source == "mobile":
                    raise
                logger.warning(
                    "mobile schedule unavailable (%s); falling back to DOM", error
                )
        return await self.schedule.get_schedule(week=week)

    async def get_today_schedule(
        self,
        week: Optional[int] = None,
        target_date: Optional[dt.date] = None,
        source: Optional[str] = None,
    ) -> list[CourseItem]:
        """Fetch courses scheduled for today (or specified date) asynchronously.

        Uses the mobile backend when the query is about the current week
        (no explicit week, and target date is today or omitted); otherwise
        falls back to the DOM parser.
        """
        if source not in (None, "mobile", "dom"):
            raise ValueError("source must be None, 'mobile' or 'dom'")
        mobile_applies = week is None and (
            target_date is None or target_date == dt.date.today()
        )
        if source == "mobile" or (source is None and mobile_applies):
            try:
                overview = curriculum_to_overview(
                    await self.mobile_jwgl.get_curriculum()
                )
                return overview.get_today_courses(target_date=target_date)
            except CampusError as error:
                if source == "mobile":
                    raise
                logger.warning(
                    "mobile schedule unavailable (%s); falling back to DOM", error
                )
        return await self.schedule.get_today_schedule(
            week=week, target_date=target_date
        )

    async def get_curriculum(self) -> list[SjdCurriculumWeek]:
        """Fetch the raw current-week curriculum grid from the mobile backend."""
        return await self.mobile_jwgl.get_curriculum()

    async def get_schedule_modes(self) -> list[SjdKbMode]:
        """Fetch available schedule time modes (节次模式) from the mobile backend."""
        return await self.mobile_jwgl.get_schedule_modes()

    async def get_exam_weeks(self) -> list[SjdKeyedValue]:
        """Fetch exam week options (考试周次) from the mobile backend."""
        return await self.mobile_jwgl.get_exam_weeks()

    async def get_exam_schedule(self) -> list[dict[str, Any]]:
        """Fetch the exam schedule asynchronously (raw dicts)."""
        return await self.mobile_jwgl.get_exam_schedule()

    async def get_exam_course_schedule(self) -> list[dict[str, Any]]:
        """Fetch exam course arrangements asynchronously (raw dicts)."""
        return await self.mobile_jwgl.get_exam_course_schedule()

    async def get_jwgl_notices(self) -> list[dict[str, Any]]:
        """Fetch JWGL notices asynchronously (raw dicts)."""
        return await self.mobile_jwgl.get_jwgl_notices()

    async def get_score_report(self) -> SjdScoreReport:
        """Fetch the mobile JWGL transcript report (termGPA) asynchronously."""
        return await self.mobile_jwgl.get_score_report()

    async def get_today_classrooms(
        self, campus_id: Optional[str] = None
    ) -> list[SjdClassroomEntry]:
        """Fetch real-time empty classrooms from the mobile backend."""
        return await self.mobile_jwgl.get_today_classrooms(campus_id=campus_id)

    async def get_notices(
        self,
        count: int = 10,
        types: Optional[list[Union[NoticeType, str]]] = None,
        page: int = 0,
    ) -> list[NoticeItem]:
        """Fetch latest portal notifications and news asynchronously."""
        return await self.portal.get_notices(count=count, types=types, page=page)

    async def get_notice_detail(
        self, url_or_item: Union[str, NoticeItem]
    ) -> NoticeDetail:
        """Fetch detailed content and attachments of a portal notice asynchronously."""
        return await self.portal.get_notice_detail(url_or_item=url_or_item)

    async def get_campus_card_balance(self) -> CampusCardBalance:
        """Fetch campus card balances (主钱包/补助) from the portal homepage."""
        return await self.portal.get_campus_card_balance()

    async def get_bytes(self, url: str, retry_on_auth_fail: bool = True) -> bytes:
        """Fetch raw bytes (e.g. an image) via the authenticated CAS session asynchronously.

        HTTP errors and login/error pages returned with HTTP 200 are rejected
        (auth problems retry once with a fresh session), so callers never
        receive auth HTML as if it were the requested resource.
        """
        if not isinstance(url, str) or not url.strip():
            raise ValueError("URL must not be empty")
        cli = await self._cas_session_mgr.ensure_cas_login()
        try:
            resp = await cli.get(url)
            problem: Optional[Exception] = None
            if resp.status_code == 401 or 300 <= resp.status_code < 400:
                problem = CampusAuthError(
                    f"Portal session expired while fetching bytes from '{url}'"
                )
            elif 200 <= resp.status_code < 300:
                problem = _bytes_page_problem(resp, f"bytes fetch from '{url}'")
            if problem is not None:
                if retry_on_auth_fail and isinstance(problem, CampusAuthError):
                    self._cas_session_mgr.invalidate()
                    return await self.get_bytes(url, retry_on_auth_fail=False)
                raise problem

            resp.raise_for_status()
            return resp.content
        except (CampusAuthError, CampusParseError):
            raise
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"Failed to fetch bytes from '{url}': {e}") from e

    async def aclose(self):
        """Close active network sessions."""
        await self._cas_session_mgr.aclose()
        await self._jwgl_session_mgr.aclose()
        await self._mobile_jwgl_session_mgr.aclose()

    async def __aenter__(self) -> "AsyncBuptClient":
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb):
        await self.aclose()
