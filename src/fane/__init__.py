"""Fane: an unobtrusive, context-aware desktop assistant for Ubuntu/GNOME."""

__version__ = "0.1.0"


def main() -> None:
    from fane.cli import main as cli_main

    cli_main()
