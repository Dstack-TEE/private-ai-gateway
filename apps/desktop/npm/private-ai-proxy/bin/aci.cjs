#!/usr/bin/env node
"use strict";

// npm's Windows shims run a bin script without the name it was invoked by, so
// the legacy `aci` command has its own entry that passes that name on.
require("./private-ai-proxy.cjs").run("aci");
