import { observer } from "mobx-react-lite";
import { Icon } from "../../components/Icon";
import { useAppearance } from "../../stores/AppearanceContext";
import { resolveMaterialFileIcon, resolveMaterialFolderIcon } from "./materialResourceIcon";

/** 资源树和面包屑共用图标选择；仅切换库提供的变体，不滤色用户内容。 */
export const ResourceMaterialIcon = observer(function ResourceMaterialIcon(props: Readonly<{
  name: string;
  kind: "file" | "folder";
  open?: boolean;
  root?: boolean;
  className: string;
  size?: number;
}>) {
  const light = useAppearance().preset.color_mode === "light";
  const url = props.kind === "file"
    ? resolveMaterialFileIcon(props.name, light)
    : resolveMaterialFolderIcon(props.name, props.open ?? false, props.root ?? false, light);
  return url ? <img alt="" aria-hidden="true" className={props.className} src={url} /> : <Icon name={props.kind} size={props.size ?? 16} />;
});
