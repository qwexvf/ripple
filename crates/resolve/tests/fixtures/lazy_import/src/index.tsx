import { lazy } from "react";
export const fields = {
  Editor: lazy(() =>
    import("./v/editor").then((m) => ({
      default: m.VRChatFieldsEditor,
    })),
  ),
};
export const other = () => import("./v/editor").then(m => m.VRChatFieldsEditor);
