"""Keeps a second copy of the stored bytes on another machine."""

import random


class MirrorLink:
    def __init__(self, pause_seconds):
        self.pause_seconds = pause_seconds
        self.tries = 0

    def resume(self, tries):
        """Picks the pause to take ahead of dialing the other machine again."""
        self.tries = tries
        pause = self.pause_seconds * min(tries, 6)
        # Random jitter keeps mirrors from dialing back in lockstep, so the
        # reconnects are spread out instead of arriving together.
        return pause + random.uniform(0, pause / 2)
