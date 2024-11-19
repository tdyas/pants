# Copyright 2024 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations

import datetime as dt
import json
import logging
import textwrap
from dataclasses import dataclass
from pathlib import Path
from typing import Any, cast

import boto3

from pants.backend.python.subsystems.aws_codeartifact import PythonAwsCodeartifact
from pants.backend.python.subsystems.repos import PythonRepos
from pants.backend.python.util_rules.pex_cli import (
    PexKeyringConfigurationRequest,
    PexKeyringConfigurationResponse,
)
from pants.base.build_root import BuildRoot
from pants.core.util_rules.environments import determine_bootstrap_environment
from pants.engine.environment import EnvironmentName
from pants.engine.internals.scheduler import Scheduler, SchedulerSession
from pants.engine.internals.selectors import Get, Params
from pants.engine.internals.session import SessionValues
from pants.engine.rules import QueryRule, _uncacheable_rule, collect_rules, rule
from pants.engine.unions import UnionRule
from pants.option.options_bootstrapper import OptionsBootstrapper

logger = logging.getLogger(__name__)


# The duration before key expiration at which renewal will be triggered.
_RENEWAL_WINDOW = dt.timedelta(minutes=30)


@dataclass(frozen=True)
class AuthToken:
    token: str
    expires: dt.datetime

    def to_json_dict(self) -> dict[str, str]:
        return {
            "token": self.token,
            "expires": self.expires.isoformat(),
        }


def _load_token() -> AuthToken | None:
    build_root: Path = BuildRoot().pathlib_path
    aws_codeartifact_dir = build_root / ".pants.d" / "aws" / "codeartifact"
    aws_codeartifact_auth_cache = aws_codeartifact_dir / "auth-cache.json"
    if not aws_codeartifact_auth_cache.exists():
        return None

    try:
        raw_data = aws_codeartifact_auth_cache.read_bytes()
        data = json.loads(raw_data)
    except Exception as e:
        logger.debug("CodeArtifact auth cache was not readable: {e}")
        return None

    if not isinstance(data, dict):
        logger.debug("CodeArtifact auth cache was not a JSON object.")
        return None

    token: Any = data.get("token")
    expires: Any = data.get("expires")
    if token is None or expires is None:
        logger.debug("CodeArtifact auth cache did not have all required fields.")
        return None

    if not isinstance(token, str) or not isinstance(expires, str):
        logger.debug("CodeArtifact auth cache did not have all required fields with correct types.")
        return None

    return AuthToken(token=token, expires=dt.datetime.fromisoformat(expires))


def _save_token(auth_token: AuthToken) -> None:
    build_root: Path = BuildRoot().pathlib_path
    aws_codeartifact_dir = build_root / ".pants.d" / "aws" / "codeartifact"
    aws_codeartifact_dir.mkdir(parents=True, exist_ok=True)

    aws_codeartifact_auth_cache = aws_codeartifact_dir / "auth-cache.json"
    data = json.dumps(auth_token.to_json_dict()).encode()
    aws_codeartifact_auth_cache.write_bytes(data)


def _codeartifact_login(domain: str) -> AuthToken:
    logger.info("Logging in to AWS CodeArtifact.")
    codeartifact = boto3.client("codeartifact")
    response = codeartifact.get_authorization_token(domain=domain)
    logger.info("Logged in to AWS CodeArtifact.")
    return AuthToken(token=response["authorizationToken"], expires=response["expiration"])


def _ensure_aws_codeartifact_login(options: PythonAwsCodeartifact) -> AuthToken:
    auth_token = _load_token()
    if auth_token is not None:
        if dt.datetime.now(dt.timezone.utc) < auth_token.expires - _RENEWAL_WINDOW:
            # TODO: Write out the key to the store used by pip invocations.
            return auth_token

    auth_token = _codeartifact_login(options.domain)
    # TODO: Error handling and retry logic.
    _save_token(auth_token)
    return auth_token


@dataclass(frozen=True)
class _AwsCodeArtifactLogin:
    auth_token: AuthToken | None


# This rule is uncacheable so that it will every session to ensure the AWS CodeArtifact token has 
# been renewed if need be.
@_uncacheable_rule
async def aws_codeartifact_login_rule(
    codeartifact_subsystem: PythonAwsCodeartifact,
) -> _AwsCodeArtifactLogin:
    if codeartifact_subsystem.enabled:
        logger.info("aws_codeartifact_login_rule: Checking token validity.")
        auth_token = _ensure_aws_codeartifact_login(codeartifact_subsystem)
        return _AwsCodeArtifactLogin(auth_token)
    else:
        logger.info("aws_codeartifact_login_rule: Disabled.")
        return _AwsCodeArtifactLogin(None)


class AwsCodeArtifactPexKeyringConfigurationRequest(PexKeyringConfigurationRequest):
    pass


@rule
async def aws_code_artifact_pex_keyring_configuration_request(
    _request: AwsCodeArtifactPexKeyringConfigurationRequest,
    codeartifiact_subsystem: PythonAwsCodeartifact,
    python_repos: PythonRepos,
) -> PexKeyringConfigurationResponse:
    logger.info(
        "aws_code_artifact_pex_keyring_configuration_request: Checking whether to include AWS keyring data."
    )
    needs_codeartifact = any(
        repo.find("codeartifact") >= 0 for repo in [*python_repos.indexes, *python_repos.find_links]
    )
    if needs_codeartifact and codeartifiact_subsystem.enabled:
        auth_token_wrapper = await Get(_AwsCodeArtifactLogin)
        auth_token = auth_token_wrapper.auth_token
        if auth_token and dt.datetime.now(dt.timezone.utc) < auth_token.expires:
            logger.info("aws_code_artifact_pex_keyring_configuration_request: YES")
            return PexKeyringConfigurationResponse(keyring_trampoline_data=auth_token.token)

    logger.info("aws_code_artifact_pex_keyring_configuration_request: Nope.")
    return PexKeyringConfigurationResponse(keyring_trampoline_data=None)


def rules():
    return [
        *collect_rules(),
        *PythonAwsCodeartifact.rules(),
        UnionRule(PexKeyringConfigurationRequest, AwsCodeArtifactPexKeyringConfigurationRequest),
        # QueryRule(SessionValues, []),
        # QueryRule(PythonAwsCodeartifact, [EnvironmentName]),
    ]
