"""Demo module summary. Second sentence."""

__all__ = ['Widget', 'helper', 'CONST', 'MyError']


class Widget:
    """A widget. It has details."""

    #: the size
    size = 1

    def __init__(self, x: int = 3):
        self.label = 'w'

    def run(self, a, b=2):
        """Run the widget."""

    @property
    def ready(self):
        """Whether ready."""


class _Hidden:
    pass


class MyError(Exception):
    pass


def helper(x: int, y=1) -> str:
    """Help with things."""


CONST = 42
UNDOC = 7
