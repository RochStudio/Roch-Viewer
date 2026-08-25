"""Roch Viewer: a read-only memory-controller and timing viewer.

Laid out by what a module talks to rather than by layer: hardware is the
access itself, intel the memory controller, memory the modules, sensors
the board, gpu the card, ui the window. The dispatcher and the profile it
dispatches on stay here, because they are what decides which of the rest
is allowed to run.
"""
