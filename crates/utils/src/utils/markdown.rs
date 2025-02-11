use markdown_it::MarkdownIt;
use once_cell::sync::Lazy;

mod spoiler_rule;

static MARKDOWN_PARSER: Lazy<MarkdownIt> = Lazy::new(|| {
  let mut parser = MarkdownIt::new();
  markdown_it::plugins::cmark::add(&mut parser);
  markdown_it::plugins::extra::add(&mut parser);
  spoiler_rule::add(&mut parser);

  parser
});

/// Replace special HTML characters in API parameters to prevent XSS attacks.
///
/// Taken from https://github.com/OWASP/CheatSheetSeries/blob/master/cheatsheets/Cross_Site_Scripting_Prevention_Cheat_Sheet.md#output-encoding-for-html-contexts
///
/// `>` is left in place because it is interpreted as markdown quote.
pub fn sanitize_html(text: &str) -> String {
  text
    .replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('\"', "&quot;")
    .replace('\'', "&#x27;")
}

/// Converts text from markdown to HTML, while escaping special characters.
pub fn markdown_to_html(text: &str) -> String {
  MARKDOWN_PARSER.parse(text).xrender()
}

#[cfg(test)]
mod tests {
  #![allow(clippy::unwrap_used)]
  #![allow(clippy::indexing_slicing)]

  use super::*;

  #[test]
  fn test_basic_markdown() {
    let tests: Vec<_> = vec![
      (
        "headings",
        "# h1\n## h2\n### h3\n#### h4\n##### h5\n###### h6",
        "<h1>h1</h1>\n<h2>h2</h2>\n<h3>h3</h3>\n<h4>h4</h4>\n<h5>h5</h5>\n<h6>h6</h6>\n"
      ),
      (
        "line breaks",
        "First\rSecond",
        "<p>First\nSecond</p>\n"),
      (
        "emphasis",
        "__bold__ **bold** *italic* ***bold+italic***",
        "<p><strong>bold</strong> <strong>bold</strong> <em>italic</em> <em><strong>bold+italic</strong></em></p>\n"
      ),
      (
        "blockquotes",
        "> #### Hello\n > \n > - Hola\n > - 안영 \n>> Goodbye\n",
        "<blockquote>\n<h4>Hello</h4>\n<ul>\n<li>Hola</li>\n<li>안영</li>\n</ul>\n<blockquote>\n<p>Goodbye</p>\n</blockquote>\n</blockquote>\n"
      ),
      (
        "lists (ordered, unordered)",
        "1. pen\n2. apple\n3. apple pen\n- pen\n- pineapple\n- pineapple pen",
        "<ol>\n<li>pen</li>\n<li>apple</li>\n<li>apple pen</li>\n</ol>\n<ul>\n<li>pen</li>\n<li>pineapple</li>\n<li>pineapple pen</li>\n</ul>\n"
      ),
      (
        "code and code blocks",
        "this is my amazing `code snippet` and my amazing ```code block```",
        "<p>this is my amazing <code>code snippet</code> and my amazing <code>code block</code></p>\n"
      ),
      (
        "links",
        "[Lemmy](https://join-lemmy.org/ \"Join Lemmy!\")",
        "<p><a href=\"https://join-lemmy.org/\" title=\"Join Lemmy!\">Lemmy</a></p>\n"
      ),
      (
        "images",
        "![My linked image](https://image.com \"image alt text\")",
        "<p><img src=\"https://image.com\" alt=\"My linked image\" title=\"image alt text\" /></p>\n"
      ),
      // Ensure any custom plugins are added to 'MARKDOWN_PARSER' implementation.
      (
        "basic spoiler",
        "::: spoiler click to see more\nhow spicy!\n:::\n",
        "<details><summary>click to see more</summary><p>how spicy!\n</p></details>\n"
      ),
      (
          "escape html special chars",
          "<script>alert('xss');</script> hello &\"",
          "<p>&lt;script&gt;alert(‘xss’);&lt;/script&gt; hello &amp;&quot;</p>\n"
          )
    ];

    tests.iter().for_each(|&(msg, input, expected)| {
      let result = markdown_to_html(input);

      assert_eq!(
        result, expected,
        "Testing {}, with original input '{}'",
        msg, input
      );
    });
  }

  #[test]
  fn test_sanitize_html() {
    let sanitized = sanitize_html("<script>alert('xss');</script> hello &\"'");
    let expected = "&lt;script>alert(&#x27;xss&#x27;);&lt;/script> hello &amp;&quot;&#x27;";
    assert_eq!(expected, sanitized)
  }
}
