// AMWAPOS owner view (read-only). Plain script: no build step, no framework.
// The token arrives in the link's #fragment (never sent to a server log),
// is kept in this phone's storage and sent as a bearer header.
(function () {
  "use strict";
  var TOKEN = "amw_companion_token";
  var SNAP = "amw_companion_snapshot";
  var LANG = "amw_companion_lang";
  var AR = {
    "Refresh": "تحديث",
    "Loading…": "جارٍ التحميل…",
    "Today": "اليوم",
    "Net sales": "صافي المبيعات",
    "Transactions": "المعاملات",
    "Average basket": "متوسط السلة",
    "Gross profit": "إجمالي الربح",
    "Refunds": "المرتجعات",
    "Pending deliveries": "التوصيلات المعلقة",
    "Low stock": "مخزون منخفض",
    "Needs attention": "يحتاج إلى متابعة",
    "Top products": "أكثر المنتجات مبيعاً",
    "Nothing pending.": "لا يوجد شيء معلق.",
    "Nothing below its reorder point.": "لا يوجد منتج تحت حد إعادة الطلب.",
    "Updated {0}": "آخر تحديث {0}",
    "The hub cannot be reached. Showing the last figures from {0}.": "تعذر الوصول إلى الخادم. تُعرض آخر الأرقام من {0}.",
    "This phone link has expired or was revoked. Ask for a new link on the hub (Admin → Phone view).":
      "انتهت صلاحية رابط الهاتف أو أُلغي. اطلب رابطاً جديداً من الخادم (الإدارة ← عرض الهاتف).",
    "Open the link from the hub (Admin → Phone view) on this phone.": "افتح الرابط من الخادم (الإدارة ← عرض الهاتف) على هذا الهاتف.",
    "The phone view is turned off on the hub.": "عرض الهاتف متوقف على الخادم.",
    "Read-only view. No sales, refunds or cash from this phone.": "عرض للقراءة فقط. لا مبيعات ولا مرتجعات ولا نقد من هذا الهاتف.",
    "pending": "قيد الانتظار",
    "preparing": "قيد التحضير",
    "dispatched": "خرج للتوصيل",
    "paid": "مدفوع",
    "cod": "الدفع عند الاستلام",
    "On hand {0} · reorder at {1}": "المتوفر {0} · إعادة الطلب عند {1}",
    "vs {0} last week": "مقابل {0} الأسبوع الماضي"
  };
  var lang = localStorage.getItem(LANG) || ((navigator.language || "").indexOf("ar") === 0 ? "ar" : "en");
  function t(s) {
    var out = lang === "ar" && AR[s] ? AR[s] : s;
    for (var i = 1; i < arguments.length; i++) out = out.replace("{" + (i - 1) + "}", String(arguments[i]));
    return out;
  }
  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined && text !== null) e.textContent = String(text);
    return e;
  }
  var digits = 3;
  var currency = "BHD";
  function money(m) {
    var neg = m < 0;
    var a = Math.abs(Number(m) || 0);
    var d = Math.pow(10, digits);
    var s = Math.floor(a / d) + (digits ? "." + String(a % d).padStart(digits, "0") : "");
    return (neg ? "−" : "") + s + " " + currency;
  }
  function qty(m) {
    var v = (Number(m) || 0) / 1000;
    return Number.isInteger(v) ? String(v) : v.toFixed(3);
  }
  function when(iso) {
    try {
      return new Date(iso).toLocaleString(lang === "ar" ? "ar-BH" : "en-GB", { dateStyle: "medium", timeStyle: "short" });
    } catch (e) {
      return iso;
    }
  }

  // Take the token from the link, then drop it from the address bar.
  var m = /[#&]t=([0-9a-f]{20,200})/.exec(location.hash || "");
  if (m) {
    localStorage.setItem(TOKEN, m[1]);
    history.replaceState(null, "", location.pathname);
  }

  function render(snap, notice, tone) {
    var app = document.getElementById("app");
    app.textContent = "";
    if (notice) app.appendChild(el("div", "banner" + (tone === "bad" ? " bad" : ""), notice));
    if (!snap) return;
    digits = snap.digits == null ? 3 : snap.digits;
    currency = snap.currency || "BHD";
    document.getElementById("biz").textContent = snap.business_name || "AMWAPOS";
    document.getElementById("stamp").textContent = t("Updated {0}", when(snap.generated_at));
    var d = snap.dashboard || {};
    var k = d.kpis || {};
    var today = el("section", "card");
    today.appendChild(el("h2", null, t("Today") + " · " + (d.business_date || "")));
    var grid = el("div", "kpis");
    function kpi(label, v, prev) {
      var c = el("div", "kpi");
      c.appendChild(el("div", "muted small", label));
      c.appendChild(el("div", "v", v));
      if (prev != null) c.appendChild(el("div", "muted small", t("vs {0} last week", prev)));
      grid.appendChild(c);
    }
    kpi(t("Net sales"), money(k.sales), money(k.sales_prev));
    kpi(t("Transactions"), k.transactions, k.transactions_prev);
    kpi(t("Average basket"), money(k.average_basket), null);
    if (k.gross_profit != null) kpi(t("Gross profit"), money(k.gross_profit), null);
    kpi(t("Refunds"), money(k.refunds) + " (" + (k.refund_count || 0) + ")", null);
    today.appendChild(grid);
    app.appendChild(today);

    if ((d.attention || []).length) {
      var att = el("section", "card");
      att.appendChild(el("h2", null, t("Needs attention")));
      d.attention.forEach(function (a) {
        att.appendChild(el("div", a.severity === "error" ? "banner bad" : "banner", a.text));
      });
      app.appendChild(att);
    }

    var del = el("section", "card");
    del.appendChild(el("h2", null, t("Pending deliveries") + " (" + (snap.deliveries || []).length + ")"));
    if (!(snap.deliveries || []).length) del.appendChild(el("div", "muted", t("Nothing pending.")));
    else {
      var tb = el("table");
      snap.deliveries.forEach(function (x) {
        var tr = el("tr");
        var a = el("td");
        a.appendChild(el("div", null, x.number + " · " + (x.customer || "")));
        a.appendChild(el("div", "muted small", (x.area || "") + " · " + when(x.created_at)));
        tr.appendChild(a);
        var b = el("td", "n");
        b.appendChild(el("div", null, money(x.amount_minor)));
        b.appendChild(el("span", "pill", t(x.status)));
        b.appendChild(el("span", "pill", t(x.payment)));
        tr.appendChild(b);
        tb.appendChild(tr);
      });
      del.appendChild(tb);
    }
    app.appendChild(del);

    var low = el("section", "card");
    low.appendChild(el("h2", null, t("Low stock") + " (" + (snap.low_stock || []).length + ")"));
    if (!(snap.low_stock || []).length) low.appendChild(el("div", "muted", t("Nothing below its reorder point.")));
    else {
      var lt = el("table");
      snap.low_stock.forEach(function (x) {
        var tr = el("tr");
        tr.appendChild(el("td", null, x.name));
        tr.appendChild(el("td", "n muted small", t("On hand {0} · reorder at {1}", qty(x.qty_milli), qty(x.reorder_point_milli))));
        lt.appendChild(tr);
      });
      low.appendChild(lt);
    }
    app.appendChild(low);

    if ((d.top_products || []).length) {
      var top = el("section", "card");
      top.appendChild(el("h2", null, t("Top products")));
      var tt = el("table");
      d.top_products.forEach(function (x) {
        var tr = el("tr");
        tr.appendChild(el("td", null, x.name));
        tr.appendChild(el("td", "n", money(x.total)));
        tt.appendChild(tr);
      });
      top.appendChild(tt);
      app.appendChild(top);
    }
  }

  function cached() {
    try {
      return JSON.parse(localStorage.getItem(SNAP) || "null");
    } catch (e) {
      return null;
    }
  }

  function load() {
    var token = localStorage.getItem(TOKEN);
    if (!token) {
      render(null, t("Open the link from the hub (Admin → Phone view) on this phone."), "bad");
      return;
    }
    fetch("/companion/api/snapshot", { headers: { Authorization: "Bearer " + token }, cache: "no-store" })
      .then(function (r) {
        if (r.status === 401) {
          localStorage.removeItem(TOKEN);
          localStorage.removeItem(SNAP);
          render(null, t("This phone link has expired or was revoked. Ask for a new link on the hub (Admin → Phone view)."), "bad");
          return null;
        }
        if (r.status === 403 || r.status === 404) {
          render(cached(), t("The phone view is turned off on the hub."), "bad");
          return null;
        }
        if (!r.ok) throw new Error("hub " + r.status);
        return r.json();
      })
      .then(function (snap) {
        if (!snap) return;
        localStorage.setItem(SNAP, JSON.stringify(snap));
        render(snap);
      })
      .catch(function () {
        var c = cached();
        render(c, c ? t("The hub cannot be reached. Showing the last figures from {0}.", when(c.generated_at)) : t("The hub cannot be reached. Showing the last figures from {0}.", "—"));
      });
  }

  function applyLang() {
    document.documentElement.lang = lang;
    document.documentElement.dir = lang === "ar" ? "rtl" : "ltr";
    document.getElementById("lang").textContent = lang === "ar" ? "English" : "العربية";
    document.getElementById("refresh").textContent = t("Refresh");
    document.getElementById("foot").textContent = t("Read-only view. No sales, refunds or cash from this phone.");
    var st = document.getElementById("status");
    if (st) st.textContent = t("Loading…");
  }
  document.getElementById("lang").addEventListener("click", function () {
    lang = lang === "ar" ? "en" : "ar";
    localStorage.setItem(LANG, lang);
    applyLang();
    load();
  });
  document.getElementById("refresh").addEventListener("click", load);
  applyLang();
  var c0 = cached();
  if (c0) render(c0);
  load();
  setInterval(load, 60000);
  if ("serviceWorker" in navigator) navigator.serviceWorker.register("/companion/sw.js", { scope: "/companion/" }).catch(function () {});
})();
