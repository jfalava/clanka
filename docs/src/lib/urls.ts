import type {
  Breadcrumb,
  PrevNext,
  SidebarItem,
} from "@cloudflare/nimbus-docs/types";

const basePrefix = import.meta.env.BASE_URL.replace(/\/$/, "");

export function withBase(href: string): string {
  if (
    !href.startsWith("/") ||
    basePrefix === "" ||
    href === basePrefix ||
    href.startsWith(`${basePrefix}/`)
  ) {
    return href;
  }

  return href === "/" ? `${basePrefix}/` : `${basePrefix}${href}`;
}

export function withBaseSidebar(items: SidebarItem[]): SidebarItem[] {
  return items.map((item) => {
    if (item.type === "group") {
      return {
        ...item,
        indexHref: item.indexHref ? withBase(item.indexHref) : undefined,
        children: withBaseSidebar(item.children),
      };
    }

    return { ...item, href: withBase(item.href) };
  });
}

export function withBaseBreadcrumbs(items: Breadcrumb[]): Breadcrumb[] {
  return items.map((item) => ({
    ...item,
    href: item.href ? withBase(item.href) : undefined,
  }));
}

export function withBasePrevNext(value: PrevNext): PrevNext {
  return {
    prev: value.prev
      ? { ...value.prev, href: withBase(value.prev.href) }
      : undefined,
    next: value.next
      ? { ...value.next, href: withBase(value.next.href) }
      : undefined,
  };
}

export function withBaseInText(
  value: string,
  site: string,
  paths: string[],
): string {
  if (basePrefix === "") {
    return value;
  }

  const siteOrigin = new URL(site).origin;

  return paths.reduce(
    (text, path) =>
      text.replaceAll(`${siteOrigin}${path}`, `${siteOrigin}${withBase(path)}`),
    value,
  );
}
