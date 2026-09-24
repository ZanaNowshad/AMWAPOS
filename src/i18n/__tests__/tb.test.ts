import { afterEach, describe, expect, it } from "vitest";
import { setLang, t, tb } from "..";

afterEach(() => setLang("en"));

describe("backend text translation", () => {
  it("is a no-op in English", () => {
    expect(tb("Barcode 123 was not found.")).toBe("Barcode 123 was not found.");
    expect(t("Shift {0}", 5)).toBe("Shift 5");
  });

  it("translates exact and parameterised backend messages in Arabic", () => {
    setLang("ar");
    expect(tb("Manager approval required.")).toBe("مطلوب موافقة المدير.");
    expect(tb("Barcode 6281007031126 was not found.")).toBe("لم يُعثر على الباركود 6281007031126.");
    expect(tb("Incorrect PIN. 3 attempt(s) left before the account is locked.")).toBe(
      "رمز سري خاطئ. تبقّى 3 محاولة قبل قفل الحساب.",
    );
    // Nested: the value is itself a known phrase.
    expect(tb("Row 7: Name is empty.")).toBe("الصف 7: الاسم فارغ.");
    // Unknown text passes through unchanged.
    expect(tb("Something entirely new")).toBe("Something entirely new");
    expect(t("Shift {0}", 5)).toBe("الوردية 5");
    expect(tb("Last successful backup 24 Sep 2026 18:43")).toBe("آخر نسخة احتياطية ناجحة 24 سبتمبر 2026 18:43");
  });
});
