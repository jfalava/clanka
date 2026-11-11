/// <reference path="../.astro/types.d.ts" />

declare module "*.astro" {
  import type { AstroComponentFactory } from "astro/runtime/server/index.js";

  const Component: AstroComponentFactory;
  export default Component;
}

declare namespace App {
  interface Locals {
    /** Per-request id counters owned by components such as PackageManagers. */
    __nbCounters?: Map<string, number>;
  }
}
