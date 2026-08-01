import {
  Archive,
  Check,
  ChevronDown,
  Compass,
  Copy,
  Ellipsis,
  History,
  GitMerge,
  ListTree,
  Paperclip,
  Pencil,
  Quote,
  Search,
  CheckCheck,
  RotateCw,
  SendHorizontal,
  Settings,
  Trash2,
  Undo2,
  Workflow,
  createIcons,
} from "lucide";

const icons = {
  Archive,
  Check,
  ChevronDown,
  Compass,
  Copy,
  Ellipsis,
  History,
  GitMerge,
  ListTree,
  Paperclip,
  Pencil,
  Quote,
  Search,
  CheckCheck,
  RotateCw,
  SendHorizontal,
  Settings,
  Trash2,
  Undo2,
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
