// Print every literal key passed to t() in src/ as a JSON array (sorted, unique).
import path from "node:path";
import ts from "typescript";
const root = process.cwd();
const cfg = ts.getParsedCommandLineOfConfigFile(path.join(root, "tsconfig.json"), {}, { ...ts.sys, onUnRecoverableConfigFileDiagnostic: () => {} });
const program = ts.createProgram(cfg.fileNames, cfg.options);
const keys = new Set();
const dynamic = [];
for (const sf of program.getSourceFiles()) {
  if (sf.isDeclarationFile || !sf.fileName.startsWith(path.join(root, "src")) || sf.fileName.includes("/i18n/")) continue;
  const visit = (n) => {
    if (ts.isCallExpression(n) && ts.isIdentifier(n.expression) && n.expression.text === "t" && n.arguments.length) {
      const a = n.arguments[0];
      if (ts.isStringLiteral(a) || ts.isNoSubstitutionTemplateLiteral(a)) keys.add(a.text);
      else dynamic.push(`${path.relative(root, sf.fileName)}:${sf.getLineAndCharacterOfPosition(a.getStart(sf)).line + 1}`);
    }
    ts.forEachChild(n, visit);
  };
  visit(sf);
}
console.log(JSON.stringify({ keys: [...keys].sort(), dynamic }, null, 1));
