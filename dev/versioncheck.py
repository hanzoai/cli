import os
import sys
import time
from pathlib import Path

import packaging.version

import dev
from dev import utils
from dev.dump import dump  # noqa: F401

VERSION_CHECK_FNAME = Path.home() / ".dev" / "caches" / "versioncheck"


def install_from_main_branch(io):
    """
    Install the latest version of dev from the main branch of the GitHub repository.
    """

    return utils.check_pip_install_extra(
        io,
        None,
        "Install the development version of dev from the main branch?",
        ["git+https://github.com/Dev-AI/dev.git"],
        self_update=True,
    )


def install_upgrade(io, latest_version=None):
    """
    Install the latest version of dev from PyPI.
    """

    if latest_version:
        new_ver_text = f"Newer dev version v{latest_version} is available."
    else:
        new_ver_text = "Install latest version of dev?"

    docker_image = os.environ.get("DEV_DOCKER_IMAGE")
    if docker_image:
        text = f"""
{new_ver_text} To upgrade, run:

    docker pull {docker_image}
"""
        io.tool_warning(text)
        return True

    success = utils.check_pip_install_extra(
        io,
        None,
        new_ver_text,
        ["dev-chat"],
        self_update=True,
    )

    if success:
        io.tool_output("Re-run dev to use new version.")
        sys.exit()

    return


def check_version(io, just_check=False, verbose=False):
    if not just_check and VERSION_CHECK_FNAME.exists():
        day = 60 * 60 * 24
        since = time.time() - os.path.getmtime(VERSION_CHECK_FNAME)
        if 0 < since < day:
            if verbose:
                hours = since / 60 / 60
                io.tool_output(f"Too soon to check version: {hours:.1f} hours")
            return

    # To keep startup fast, avoid importing this unless needed
    import requests

    try:
        response = requests.get("https://pypi.org/pypi/dev-chat/json")
        data = response.json()
        latest_version = data["info"]["version"]
        current_version = dev.__version__

        if just_check or verbose:
            io.tool_output(f"Current version: {current_version}")
            io.tool_output(f"Latest version: {latest_version}")

        is_update_available = packaging.version.parse(latest_version) > packaging.version.parse(
            current_version
        )
    except Exception as err:
        io.tool_error(f"Error checking pypi for new version: {err}")
        return False
    finally:
        VERSION_CHECK_FNAME.parent.mkdir(parents=True, exist_ok=True)
        VERSION_CHECK_FNAME.touch()

    ###
    # is_update_available = True

    if just_check or verbose:
        if is_update_available:
            io.tool_output("Update available")
        else:
            io.tool_output("No update available")

    if just_check:
        return is_update_available

    if not is_update_available:
        return False

    install_upgrade(io, latest_version)
    return True
