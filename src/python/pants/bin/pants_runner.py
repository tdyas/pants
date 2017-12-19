# coding=utf-8
# Copyright 2015 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import (absolute_import, division, generators, nested_scopes, print_function,
                        unicode_literals, with_statement)

import logging
import os
import sys

from pyannotate_runtime import collect_types

from pants.base.exiter import Exiter
from pants.bin.remote_pants_runner import RemotePantsRunner
from pants.option.options_bootstrapper import OptionsBootstrapper


logger = logging.getLogger(__name__)


class ExiterProxy(Exiter):
  def __init__(self, underlying, callback):
    super(ExiterProxy, self).__init__()
    self._underlying = underlying
    self._callback = callback

  def exit(self, result=0, msg=None, out=None):
    try:
      self._callback()
    except Exception:
      pass
    self._underlying.exit(result=result, msg=msg, out=out)


class PantsRunner(object):
  """A higher-level runner that delegates runs to either a LocalPantsRunner or RemotePantsRunner."""

  def __init__(self, exiter, args=None, env=None):
    """
    :param Exiter exiter: The Exiter instance to use for this run.
    :param list args: The arguments (sys.argv) for this run. (Optional, default: sys.argv)
    :param dict env: The environment for this run. (Optional, default: os.environ)
    """
    self._exiter = ExiterProxy(exiter, self._write_stats)
    self._args = args or sys.argv
    self._env = env or os.environ

  def _write_stats(self):
    collect_types.dump_stats('type_info.json')

  def run(self):
    collect_types.init_types_collection()
    result = self.run1()
    return result

  def run1(self):
    with collect_types.collect():
      options_bootstrapper = OptionsBootstrapper(env=self._env, args=self._args)
      bootstrap_options = options_bootstrapper.get_bootstrap_options()

    if bootstrap_options.for_global_scope().enable_pantsd:
      try:
        return RemotePantsRunner(self._exiter, self._args, self._env, bootstrap_options).run()
      except RemotePantsRunner.Fallback as e:
        logger.debug('caught client exception: {!r}, falling back to non-daemon mode'.format(e))

    # N.B. Inlining this import speeds up the python thin client run by about 100ms.
    from pants.bin.local_pants_runner import LocalPantsRunner

    return LocalPantsRunner(self._exiter,
                            self._args,
                            self._env,
                            options_bootstrapper=options_bootstrapper).run()
