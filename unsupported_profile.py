"""Safe neutral profile used when platform detection is inconclusive."""

EM_DASH = "—"

TIMINGS = [
    {
        "name": "Platform",
        "value": "Unsupported",
        "Category": "General",
        "Tab": "System Info",
        "Column": "Left",
    },
    {
        "name": "Read Status",
        "value": "Unsupported platform — privileged timing reads disabled",
        "Category": "General",
        "Tab": "System Info",
        "Column": "Left",
    },
]


def apply_formula(value, formula=None):
    if value is None:
        return EM_DASH
    return formula(value) if callable(formula) else value
