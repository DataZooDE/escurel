// Webviews theme with --vscode-* tokens only (SPEC §1). Any literal colour
// in webview/ is a bug: hex, rgb()/rgba(), hsl()/hsla().
const COLOUR = /#[0-9a-f]{3,8}\b|\b(?:rgb|hsl)a?\(/i;

export default {
  meta: {
    type: 'problem',
    docs: { description: 'disallow literal colours in webview code; use --vscode-* tokens' },
    messages: { literal: 'Literal colour "{{text}}" — use a --vscode-* theme token instead.' },
    schema: [],
  },
  create(context) {
    const check = (node, text) => {
      const m = COLOUR.exec(text);
      if (m) context.report({ node, messageId: 'literal', data: { text: m[0] } });
    };
    return {
      Literal(node) {
        if (typeof node.value === 'string') check(node, node.value);
      },
      TemplateElement(node) {
        check(node, node.value.raw);
      },
    };
  },
};
