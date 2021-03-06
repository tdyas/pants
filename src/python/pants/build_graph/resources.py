# Copyright 2014 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from pants.build_graph.files import Files


class Resources(Files):
    """Resource files.

    Looking for loose files in your JVM application bundle? Those are `bundle <#bundle>`_\\s.

    Resources are files included in deployable units like Java jars or Python wheels and accessible
    via language-specific APIs.

    :API: public
    """

    @classmethod
    def alias(cls):
        return "resources"
