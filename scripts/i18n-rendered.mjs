// Report string literals that reach rendered JSX text (any case) without t().
import path from "node:path";
import ts from "typescript";
const root = process.cwd();
const cfg = ts.getParsedCommandLineOfConfigFile(path.join(root, "tsconfig.json"), {}, { ...ts.sys, onUnRecoverableConfigFileDiagnostic: () => {} });
const program = ts.createProgram(cfg.fileNames, cfg.options);
program.getTypeChecker();
const SHOWN_ATTRS = new Set(["label", "title", "aria-label", "placeholder", "subtitle", "hint", "confirmLabel", "alt"]);
let n = 0;
for (const sf of program.getSourceFiles()) {
  const f = sf.fileName;
  if (sf.isDeclarationFile || !f.startsWith(path.join(root, "src")) || /__tests__|\/i18n\//.test(f) || !f.endsWith(".tsx")) continue;
  const visit = (node) => {
    if ((ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node) || ts.isTemplateHead(node) || ts.isTemplateMiddle(node) || ts.isTemplateTail(node)) && /[A-Za-z]{2,}/.test(node.text)) {
      // Walk up through value-producing expressions only.
      let q = ts.isTemplateHead(node) || ts.isTemplateMiddle(node) || ts.isTemplateTail(node) ? node.parent.parent : node;
      let shown = false;
      for (let p = q.parent; p; q = p, p = p.parent) {
        if (ts.isCallExpression(p)) break; // argument to a call (t(), api, etc.)
        if (ts.isJsxAttribute(p)) { shown = SHOWN_ATTRS.has(p.name.getText(sf)); break; }
        if (ts.isJsxExpression(p)) { if (ts.isJsxElement(p.parent) || ts.isJsxFragment(p.parent)) { shown = true; break; } continue; }
        if (ts.isConditionalExpression(p) && q !== p.condition) continue;
        if (ts.isBinaryExpression(p) && q === p.right && [ts.SyntaxKind.AmpersandAmpersandToken, ts.SyntaxKind.BarBarToken, ts.SyntaxKind.QuestionQuestionToken, ts.SyntaxKind.PlusToken].includes(p.operatorToken.kind)) continue;
        if (ts.isBinaryExpression(p) && q === p.left && [ts.SyntaxKind.BarBarToken, ts.SyntaxKind.QuestionQuestionToken, ts.SyntaxKind.PlusToken].includes(p.operatorToken.kind)) continue;
        if (ts.isParenthesizedExpression(p) || ts.isTemplateSpan(p) || ts.isTemplateExpression(p) || ts.isAsExpression(p)) continue;
        break;
      }
      if (shown) {
        const { line } = sf.getLineAndCharacterOfPosition(node.getStart(sf));
        console.log(`${path.relative(root, f)}:${line + 1}: ${JSON.stringify(node.text)}`);
        n++;
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
}
console.log(`rendered literals without t(): ${n}`);
