import { Languages } from "lucide-react";
import { getLang, switchLang } from "../i18n";
import { Button } from "./ui";

/** English ⇄ العربية. Per computer; the page reloads and the session is kept. */
export function LanguageToggle() {
  const ar = getLang() === "ar";
  return (
    <Button
      variant="ghost"
      size="sm"
      icon={<Languages size={15} />}
      onClick={() => switchLang(ar ? "en" : "ar")}
      data-testid="lang-toggle"
      lang={ar ? "en" : "ar"}
    >
      {ar ? "English" : "العربية"}
    </Button>
  );
}
