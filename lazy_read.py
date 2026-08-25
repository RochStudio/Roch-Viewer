"""Lazy Intel hardware-reader shim.

Importing this module does not load inpoutx64. The legacy reader is imported
only if a selected timing row actually asks for an Intel MCHBAR read.
"""


def read_timing(*args, **kwargs):
    from read import read_timing as implementation

    return implementation(*args, **kwargs)
