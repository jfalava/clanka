export interface EntryData {
  readonly socialImage?: string;
}

export function isEntryData(value: unknown): value is EntryData {
  if (typeof value !== "object" || value === null) {
    return false;
  }

  const image: unknown = Reflect.get(value, "socialImage");

  return image === undefined || typeof image === "string";
}

export function entrySocialImage(data: EntryData): string | undefined {
  const image = data.socialImage;

  return image !== undefined && image.length > 0 ? image : undefined;
}
