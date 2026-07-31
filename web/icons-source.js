import {
  Archive,
  ChevronDown,
  Compass,
  Ellipsis,
  History,
  ListTree,
  Paperclip,
  SendHorizontal,
  Settings,
  Workflow,
  createIcons,
} from "lucide";

const icons = {
  Archive,
  ChevronDown,
  Compass,
  Ellipsis,
  History,
  ListTree,
  Paperclip,
  SendHorizontal,
  Settings,
  Workflow,
};

export function renderIcons(root = document) {
  createIcons({
    icons,
    root,
    inTemplates: true,
    attrs: {
      "aria-hidden": "true",
      focusable: "false",
      "stroke-width": "1.8",
    },
  });
}
