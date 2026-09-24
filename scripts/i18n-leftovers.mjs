// List string literals that look like English UI text and are not passed to t().
import path from "node:path";
import ts from "typescript";
const root = process.cwd();
const cfg = ts.getParsedCommandLineOfConfigFile(path.join(root, "tsconfig.json"), {}, { ...ts.sys, onUnRecoverableConfigFileDiagnostic: () => {} });
const program = ts.createProgram(cfg.fileNames, cfg.options);
program.getTypeChecker(); // binds parent pointers
const NON_DISPLAY_ATTRS = new Set(["className", "tone", "variant", "size", "value", "type", "role", "inputMode", "id", "htmlFor", "data-testid", "autoComplete", "dir", "fieldClass", "color", "summary", "k", "target", "rel", "accept", "fill", "textAnchor", "preserveAspectRatio", "key", "name", "icon", "method", "align", "aria-modal", "aria-live", "aria-haspopup", "aria-busy", "data-line"]);
const looksText = (s) => /[A-Za-z]{2,}/.test(s) && /[A-Z][a-z]|[a-z] [a-z]|[a-z]{3,} /.test(s) && !/^[a-z0-9_.:/-]+$/.test(s) && !/^(var\(|#|rgba?\(|calc\()/.test(s) && !/^[\w-]+(\s[\w-]+)*$/.test(s.trim()) || /^[A-Z][a-z]+( [A-Za-z]+)*[.…:?!]?$/.test(s);
const skip = [/src\/i18n\//, /__tests__/, /src\/test\//, /src\/api\/types\.ts$/, /vite-env/];
let n = 0;
for (const sf of program.getSourceFiles()) {
  const f = sf.fileName;
  if (sf.isDeclarationFile || !f.startsWith(path.join(root, "src")) || skip.some((r) => r.test(f))) continue;
  const visit = (node) => {
    if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node) || ts.isTemplateHead(node)) {
      const s = node.text;
      let p = node.parent;
      let inT = false;
      for (let q = node.parent; q; q = q.parent) {
        if (ts.isCallExpression(q) && ts.isIdentifier(q.expression) && ["t", "ltr"].includes(q.expression.text)) { inT = true; break; }
        if (ts.isImportDeclaration(q) || ts.isExportDeclaration(q)) { inT = true; break; }
      }
      if (ts.isTemplateHead(node)) p = node.parent.parent;
      const attr = ts.isJsxAttribute(p) ? p.name.getText(sf) : ts.isJsxExpression(p) && ts.isJsxAttribute(p.parent) ? p.parent.name.getText(sf) : null;
      const isClass = attr && NON_DISPLAY_ATTRS.has(attr);
      const inElemAccess = ts.isElementAccessExpression(p) || (ts.isPropertyAssignment(p) && p.name === node) || ts.isLiteralTypeNode(p) || ts.isCaseClause(p);
      const cmp = ts.isBinaryExpression(p) && [ts.SyntaxKind.EqualsEqualsEqualsToken, ts.SyntaxKind.ExclamationEqualsEqualsToken].includes(p.operatorToken.kind);
      if (!inT && !isClass && !inElemAccess && !cmp && looksText(s)) {
        const { line } = sf.getLineAndCharacterOfPosition(node.getStart(sf));
        console.log(`${path.relative(root, f)}:${line + 1}: ${JSON.stringify(s)}`);
        n++;
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
}
console.log(`leftovers: ${n}`);
