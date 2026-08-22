/**
 * opencode plugin entry for the neutral ACI provider.
 *
 * opencode's plugin loader treats EVERY runtime export as a plugin and calls
 * it, so this entry must export nothing but the plugin function. The library
 * surface (createProvider, types, verifier helpers) lives in ./core.ts and is
 * importable as `@phala/opencode-provider-aci/core`.
 */
import { createProvider } from "./core.ts";

export default createProvider();
