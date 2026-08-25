"""Platform timing-backend dispatcher.

Only the selected platform module is imported.  This is the safety boundary
that prevents Intel MCHBAR installers/readers from running on AMD hardware.
"""

from platform_profiles import (
    LGA1700_DDR4,
    LGA1700_DDR5,
    LGA1851,
    detect_current_platform,
)


def load_timing_backend(profile):
    if profile in (LGA1700_DDR4, LGA1700_DDR5, LGA1851):
        from intel_timings import TIMINGS, apply_formula

        return TIMINGS, apply_formula
    from unsupported_profile import TIMINGS, apply_formula

    return TIMINGS, apply_formula


ACTIVE_PLATFORM = detect_current_platform()
TIMINGS, apply_formula = load_timing_backend(ACTIVE_PLATFORM)
