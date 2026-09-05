import material_icon_manifest from "material-icon-theme/dist/material-icons.json";

type MaterialIconManifest = Readonly<{
  file?: string;
  folder?: string;
  folderExpanded?: string;
  rootFolder?: string;
  rootFolderExpanded?: string;
  folderNames?: Readonly<Record<string, string>>;
  folderNamesExpanded?: Readonly<Record<string, string>>;
  rootFolderNames?: Readonly<Record<string, string>>;
  rootFolderNamesExpanded?: Readonly<Record<string, string>>;
  iconDefinitions?: Readonly<Record<string, Readonly<{ iconPath?: string }>>>;
  fileNames?: Readonly<Record<string, string>>;
  fileExtensions?: Readonly<Record<string, string>>;
  light?: Readonly<{
    fileNames?: Readonly<Record<string, string>>;
    fileExtensions?: Readonly<Record<string, string>>;
    folderNames?: Readonly<Record<string, string>>;
    folderNamesExpanded?: Readonly<Record<string, string>>;
    rootFolderNames?: Readonly<Record<string, string>>;
    rootFolderNamesExpanded?: Readonly<Record<string, string>>;
  }>;
}>;

const manifest = material_icon_manifest as MaterialIconManifest;
const icon_assets = import.meta.glob("/node_modules/material-icon-theme/icons/*.svg", {
  eager: true,
  import: "default",
  query: "?url",
}) as Readonly<Record<string, string>>;

export function resolveMaterialFileIcon(file_name: string): string | null {
  const normalized_name = file_name.toLowerCase();
  const exact_icon = manifest.light?.fileNames?.[normalized_name]
    ?? manifest.fileNames?.[normalized_name];
  if (exact_icon) return resolveIconAsset(exact_icon);

  const parts = normalized_name.split(".");
  for (let index = 1; index < parts.length; index += 1) {
    const extension = parts.slice(index).join(".");
    const extension_icon = manifest.light?.fileExtensions?.[extension]
      ?? manifest.fileExtensions?.[extension];
    if (extension_icon) return resolveIconAsset(extension_icon);
  }
  return manifest.file ? resolveIconAsset(manifest.file) : null;
}

export function resolveMaterialFolderIcon(
  folder_name: string,
  open: boolean,
  root: boolean,
): string | null {
  const normalized_name = folder_name.toLowerCase();
  const icon_id = root
    ? resolveRootFolderIcon(normalized_name, open)
    : resolveFolderIcon(normalized_name, open);
  return icon_id ? resolveIconAsset(icon_id) : null;
}

function resolveFolderIcon(folder_name: string, open: boolean): string | undefined {
  if (open) {
    return manifest.light?.folderNamesExpanded?.[folder_name]
      ?? manifest.folderNamesExpanded?.[folder_name]
      ?? manifest.folderExpanded;
  }
  return manifest.light?.folderNames?.[folder_name]
    ?? manifest.folderNames?.[folder_name]
    ?? manifest.folder;
}

function resolveRootFolderIcon(folder_name: string, open: boolean): string | undefined {
  if (open) {
    return manifest.light?.rootFolderNamesExpanded?.[folder_name]
      ?? manifest.rootFolderNamesExpanded?.[folder_name]
      ?? manifest.rootFolderExpanded;
  }
  return manifest.light?.rootFolderNames?.[folder_name]
    ?? manifest.rootFolderNames?.[folder_name]
    ?? manifest.rootFolder;
}

function resolveIconAsset(icon_id: string): string | null {
  const icon_path = manifest.iconDefinitions?.[icon_id]?.iconPath;
  const asset_name = icon_path?.split("/").at(-1);
  return asset_name
    ? icon_assets[`/node_modules/material-icon-theme/icons/${asset_name}`] ?? null
    : null;
}
