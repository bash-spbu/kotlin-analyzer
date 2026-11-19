//! Kotlin's source maps (JSR-45 `SourceDebugExtension`). An inline
//! function's body is compiled into each caller: its lines get numbers past
//! the end of the caller's file, and the class's map tells where they come
//! from — the `Kotlin` stratum maps every line to a file and line, the
//! `KotlinDebug` stratum maps an inlined body's lines to the line of the
//! call. Lambdas passed to inline functions keep their own lines.
//!
//! ```text
//! SMAP
//! Main.kt
//! Kotlin
//! *S Kotlin
//! *F
//! + 1 Main.kt
//! demo/MainKt
//! + 2 _Collections.kt
//! kotlin/collections/CollectionsKt___CollectionsKt
//! *L
//! 1#1,31:1          lines 1–31 of Main.kt are lines 1–31
//! 2199#2,2:33       lines 2199–2200 of _Collections.kt are 33–34
//! *S KotlinDebug
//! *F
//! + 1 Main.kt
//! demo/MainKt
//! *L
//! 23#1:33,2         lines 33–34 were inlined at line 23
//! *E
//! ```

/// A file of a stratum: `_Collections.kt` at
/// `kotlin/collections/CollectionsKt___CollectionsKt`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MappedFile {
    id: u32,
    pub name: String,
    /// The JVM name of a class compiled from it, `/`-separated.
    pub path: Option<String>,
}

impl MappedFile {
    /// `kotlin.collections` of `kotlin/collections/CollectionsKt…`.
    pub fn package(&self) -> String {
        self.path
            .as_deref()
            .and_then(|path| path.rsplit_once('/'))
            .map_or_else(String::new, |(package, _)| package.replace('/', "."))
    }
}

/// `InputStartLine#FileId,RepeatCount:OutputStartLine,OutputLineIncrement`:
/// input line `input_start + i` is output lines
/// `output_start + i·increment …` (`increment` of them, at least one).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct LineRange {
    file: u32,
    input_start: u32,
    repeat: u32,
    output_start: u32,
    increment: u32,
}

impl LineRange {
    fn input_of(&self, output: u32) -> Option<u32> {
        let step = self.increment.max(1);
        let end = self.output_start + self.repeat * step;
        let inside = output >= self.output_start && output < end;
        inside.then(|| self.input_start + (output - self.output_start) / step)
    }

    fn outputs_of(&self, input: u32) -> std::ops::Range<u32> {
        let inside = input >= self.input_start && input < self.input_start + self.repeat;
        if !inside {
            return 0..0;
        }
        let step = self.increment.max(1);
        let start = self.output_start + (input - self.input_start) * step;
        start..start + step
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct Stratum {
    files: Vec<MappedFile>,
    lines: Vec<LineRange>,
}

impl Stratum {
    fn file(&self, id: u32) -> Option<&MappedFile> {
        self.files.iter().find(|file| file.id == id)
    }
}

/// Where a line of a class's code comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin<'map> {
    /// The class's own file, at `line`.
    Own { line: u32 },
    /// An inline function's body — `file` at `line` — inlined at
    /// `call_line` of the class's own file.
    Inlined {
        file: &'map MappedFile,
        line: u32,
        call_line: u32,
    },
    /// Another file's code that isn't an inlined body (the compiler's
    /// markers, `fake.kt`).
    Elsewhere { file: &'map MappedFile, line: u32 },
}

/// A class's source map: its `Kotlin` and `KotlinDebug` strata.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SourceMap {
    kotlin: Stratum,
    call_sites: Stratum,
}

impl SourceMap {
    /// The map of a `SourceDebugExtension`; `None` for anything else.
    pub fn parse(text: &str) -> Option<SourceMap> {
        let mut lines = text.lines();
        if lines.next()?.trim() != "SMAP" {
            return None;
        }
        let _output_file = lines.next()?;
        let default_stratum = lines.next()?.trim().to_owned();
        let mut strata: Vec<(String, Stratum)> = Vec::new();
        let mut section = Section::None;
        let mut pending_file: Option<MappedFile> = None;
        let mut current_file = 1;
        for line in lines {
            let line = line.trim_end();
            if let Some(name) = line.strip_prefix("*S ") {
                strata.push((name.trim().to_owned(), Stratum::default()));
                section = Section::None;
                continue;
            }
            match line {
                "*F" => {
                    section = Section::Files;
                    continue;
                }
                "*L" => {
                    section = Section::Lines;
                    current_file = 1;
                    continue;
                }
                "*E" => break,
                _ if line.starts_with('*') => {
                    section = Section::None;
                    continue;
                }
                _ => {}
            }
            let Some((_, stratum)) = strata.last_mut() else {
                continue;
            };
            match section {
                Section::Files => {
                    if let Some(mut file) = pending_file.take() {
                        file.path = Some(line.trim().to_owned());
                        stratum.files.push(file);
                        continue;
                    }
                    let (has_path, entry) = match line.strip_prefix("+ ") {
                        Some(entry) => (true, entry),
                        None => (false, line),
                    };
                    let (id, name) = entry.trim().split_once(' ')?;
                    let file = MappedFile {
                        id: id.parse().ok()?,
                        name: name.trim().to_owned(),
                        path: None,
                    };
                    if has_path {
                        pending_file = Some(file);
                    } else {
                        stratum.files.push(file);
                    }
                }
                Section::Lines => {
                    let range = parse_line_range(line, current_file)?;
                    current_file = range.file;
                    stratum.lines.push(range);
                }
                Section::None => {}
            }
        }
        let mut kotlin = None;
        let mut call_sites = Stratum::default();
        for (name, stratum) in strata {
            if name == default_stratum {
                kotlin = Some(stratum);
            } else if name == "KotlinDebug" {
                call_sites = stratum;
            }
        }
        Some(SourceMap {
            kotlin: kotlin?,
            call_sites,
        })
    }

    /// The class's own file: the first of the default stratum.
    fn own_file(&self) -> Option<&MappedFile> {
        self.kotlin.files.first()
    }

    /// Where the class's `output` line comes from; `None` past the map.
    pub fn origin(&self, output: u32) -> Option<Origin<'_>> {
        let (file_id, line) = self
            .kotlin
            .lines
            .iter()
            .find_map(|range| Some((range.file, range.input_of(output)?)))?;
        let file = self.kotlin.file(file_id)?;
        let call_line = self
            .call_sites
            .lines
            .iter()
            .find_map(|range| range.input_of(output));
        let is_own = self.own_file().is_some_and(|own| own.id == file.id);
        Some(match call_line {
            Some(call_line) => Origin::Inlined {
                file,
                line,
                call_line,
            },
            None if is_own => Origin::Own { line },
            None => Origin::Elsewhere { file, line },
        })
    }

    /// The class's lines holding `line` of the file `file_name` in
    /// `package`: its own lines for its own file, and every place an
    /// inline function's body from it was inlined.
    pub fn output_lines_of(&self, file_name: &str, package: &str, line: u32) -> Vec<u32> {
        let matching: Vec<u32> = self
            .kotlin
            .files
            .iter()
            .filter(|file| file.name == file_name && file.package() == package)
            .map(|file| file.id)
            .collect();
        let mut outputs: Vec<u32> = self
            .kotlin
            .lines
            .iter()
            .filter(|range| matching.contains(&range.file))
            .flat_map(|range| range.outputs_of(line))
            .collect();
        outputs.sort_unstable();
        outputs.dedup();
        outputs
    }

    /// Whether the map has code of `file_name` in `package`.
    pub fn mentions(&self, file_name: &str, package: &str) -> bool {
        self.kotlin
            .files
            .iter()
            .any(|file| file.name == file_name && file.package() == package)
    }
}

enum Section {
    None,
    Files,
    Lines,
}

/// `2199#3,2:33,1`; the file id carries over from the line before.
fn parse_line_range(line: &str, current_file: u32) -> Option<LineRange> {
    let (input, output) = line.trim().split_once(':')?;
    let (input, repeat) = match input.split_once(',') {
        Some((input, repeat)) => (input, repeat.parse().ok()?),
        None => (input, 1),
    };
    let (input_start, file) = match input.split_once('#') {
        Some((start, file)) => (start, file.parse().ok()?),
        None => (input, current_file),
    };
    let (output_start, increment) = match output.split_once(',') {
        Some((start, increment)) => (start, increment.parse().ok()?),
        None => (output, 1),
    };
    Some(LineRange {
        file,
        input_start: input_start.parse().ok()?,
        repeat,
        output_start: output_start.parse().ok()?,
        increment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What kotlinc 2.4 writes for a `main` calling `items.forEach { }` at
    /// line 23 and the same file's `inline fun twice` (lines 6–8) at 26.
    const MAIN: &str = "SMAP\nMain.kt\nKotlin\n*S Kotlin\n*F\n+ 1 Main.kt\ndemo/MainKt\n\
        + 2 fake.kt\nkotlin/jvm/internal/FakeKt\n+ 3 _Collections.kt\n\
        kotlin/collections/CollectionsKt___CollectionsKt\n*L\n1#1,31:1\n6#1,3:35\n1#2:32\n\
        2199#3,2:33\n*S KotlinDebug\n*F\n+ 1 Main.kt\ndemo/MainKt\n*L\n26#1:35,3\n23#1:33,2\n*E\n";

    #[test]
    fn lines_map_to_their_own_file_an_inlined_body_or_elsewhere() {
        let map = SourceMap::parse(MAIN).unwrap();
        assert_eq!(map.origin(24), Some(Origin::Own { line: 24 }));
        let Some(Origin::Inlined {
            file,
            line,
            call_line,
        }) = map.origin(34)
        else {
            panic!("{:?}", map.origin(34));
        };
        assert_eq!(
            (file.name.as_str(), file.package(), line, call_line),
            ("_Collections.kt", "kotlin.collections".to_owned(), 2200, 23)
        );
        let Some(Origin::Inlined {
            file,
            line,
            call_line,
        }) = map.origin(36)
        else {
            panic!("{:?}", map.origin(36));
        };
        assert_eq!((file.name.as_str(), line, call_line), ("Main.kt", 7, 26));
        assert!(matches!(
            map.origin(32),
            Some(Origin::Elsewhere { line: 1, .. })
        ));
        assert_eq!(map.origin(99), None);
    }

    #[test]
    fn a_files_line_is_its_own_line_and_each_place_it_was_inlined() {
        let map = SourceMap::parse(MAIN).unwrap();
        assert_eq!(map.output_lines_of("Main.kt", "demo", 7), [7, 36]);
        assert_eq!(map.output_lines_of("Main.kt", "demo", 20), [20]);
        assert_eq!(
            map.output_lines_of("_Collections.kt", "kotlin.collections", 2199),
            [33]
        );
        assert!(map.output_lines_of("Main.kt", "other", 7).is_empty());
        assert!(map.mentions("_Collections.kt", "kotlin.collections"));
        assert!(!map.mentions("Order.kt", "demo"));
    }

    #[test]
    fn ranges_repeat_with_increments_and_carry_the_file_over() {
        let map = SourceMap::parse(
            "SMAP\nA.kt\nKotlin\n*S Kotlin\n*F\n1 A.kt\n+ 2 B.kt\nb/BKt\n*L\n\
             1,10:1\n5#2,2:20,3\n9:30\n*E\n",
        )
        .unwrap();
        // 5#2,2:20,3: B.kt's line 5 is 20–22, line 6 is 23–25.
        assert_eq!(map.output_lines_of("B.kt", "b", 6), [23, 24, 25]);
        // `9:30` is still file 2.
        assert!(matches!(
            map.origin(30),
            Some(Origin::Elsewhere { line: 9, file }) if file.name == "B.kt"
        ));
        assert_eq!(map.origin(3), Some(Origin::Own { line: 3 }));
        assert_eq!(SourceMap::parse("not a map"), None);
    }
}
