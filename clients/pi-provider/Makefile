# Pack branded pi-provider artifacts from this SoT monorepo.
#
# Targets write standalone package roots under clients/release/ by default.
# Each artifact supports: npm install --omit=dev && pi -e .

ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
REPO_ROOT := $(abspath $(ROOT)/../..)
RELEASE_ROOT ?= $(REPO_ROOT)/clients/release
PACK := node $(ROOT)/scripts/pack-brand.mjs

.PHONY: help pack pack-phala-cloud pack-redpill stage smoke-phala-cloud smoke-redpill

help:
	@echo "Targets:"
	@echo "  make pack                 pack phala-cloud + redpill into $(RELEASE_ROOT)"
	@echo "  make pack-phala-cloud     pack only phala-cloud"
	@echo "  make pack-redpill         pack only redpill"
	@echo "  make stage                pack without npm install / smoke"
	@echo "  make smoke-phala-cloud    pack + npm install + pi -e . smoke"
	@echo "  make smoke-redpill        pack + npm install + pi -e . smoke"

pack:
	$(PACK) --brand all --out-root "$(RELEASE_ROOT)"

pack-phala-cloud:
	$(PACK) --brand phala-cloud --out "$(RELEASE_ROOT)/pi-provider-phala-cloud"

pack-redpill:
	$(PACK) --brand redpill --out "$(RELEASE_ROOT)/pi-provider-redpill"

stage:
	$(PACK) --brand all --out-root "$(RELEASE_ROOT)" --skip-install --skip-smoke

smoke-phala-cloud: pack-phala-cloud

smoke-redpill: pack-redpill
