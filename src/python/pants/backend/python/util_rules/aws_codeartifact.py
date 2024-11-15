# Copyright 2024 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations
import json
import logging
from pathlib import Path
import datetime as dt
from typing import Any, cast

import boto3
from pants.backend.python.subsystems.aws_codeartifact import PythonAwsCodeartifact
from pants.base.build_root import BuildRoot
from dataclasses import dataclass
from pants.core.util_rules.environments import determine_bootstrap_environment

from pants.engine.environment import EnvironmentName
from pants.engine.internals.scheduler import Scheduler, SchedulerSession
from pants.engine.internals.selectors import Params
from pants.engine.internals.session import SessionValues
from pants.engine.rules import QueryRule

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


def _load_tokem() -> AuthToken | None:
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
    codeartifact = boto3.client('codeartifact')
    response = codeartifact.get_authorization_token(domain=domain)
    logger.info("Logged in to AWS CodeArtifact.")
    return AuthToken(token=response["authorizationToken"], expires=response["expiration"])


def _ensure_aws_codeartifact_login(options: PythonAwsCodeartifact) -> None:
    auth_token = _load_tokem()
    if auth_token is not None:
        if dt.datetime.now(dt.timezone.utc) < auth_token.expires - _RENEWAL_WINDOW:
            # TODO: Write out the key to the store used by pip invocations.
            return
        
    auth_token = _codeartifact_login(options.domain)
    # TODO: Error handling and retry logic.
    _save_token(auth_token)
        


def aws_codeartifact_session_startup_hook(scheduler_session: SchedulerSession) -> None:
    env_name = determine_bootstrap_environment(scheduler_session)
    result = scheduler_session.product_request(PythonAwsCodeartifact, [Params(env_name)])
    assert len(result) == 1
    options = cast(PythonAwsCodeartifact, result[0])

    if options.enabled:
        _ensure_aws_codeartifact_login(options)


def rules():
    return [
        *PythonAwsCodeartifact.rules(),
        QueryRule(PythonAwsCodeartifact, [EnvironmentName]),
    ]
