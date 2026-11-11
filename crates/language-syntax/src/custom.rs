//! Structural metadata for a plugin grammar with a neutral toolkit tag.
//! Parsed contexts own opacity; declarations own the bracket alphabet.
use openwebide_core::{editor::MAX_BRACKETS, plugins::languages as protocol};

pub(super) fn brackets(
    source: &str,
    data: &mut protocol::SyntaxAnalysis,
    configuration: &protocol::LanguageConfiguration,
) -> Option<()> {
    let Some(structure) = &mut data.structure else {
        return Some(());
    };
    let mut opaque: Vec<_> = structure
        .protected
        .iter()
        .map(|region| region.range)
        .chain(structure.scopes.iter().map(|scope| scope.range))
        .collect();
    opaque.sort_by_key(|range| (range.start, range.end));
    let mut opaque_index = 0;
    let mut position = 0;
    let mut stack = Vec::<(usize, char)>::new();
    structure.brackets.clear();
    while position < source.len() {
        while opaque
            .get(opaque_index)
            .is_some_and(|range| range.end <= position)
        {
            opaque_index += 1;
        }
        if let Some(range) = opaque.get(opaque_index)
            && range.start <= position
        {
            position = range.end;
            continue;
        }
        let character = source[position..].chars().next()?;
        let opening = configuration
            .brackets
            .iter()
            .find(|pair| pair.open == character);
        let closing = configuration
            .brackets
            .iter()
            .any(|pair| pair.close == character);
        if opening.is_some() || closing {
            if structure.brackets.len() == MAX_BRACKETS {
                return None;
            }
            let index = structure.brackets.len();
            let mut bracket = protocol::Bracket {
                offset: position,
                character,
                partner: None,
            };
            if let Some(pair) = opening {
                stack.push((index, pair.close));
            } else if let Some((opening, expected)) = stack.last().copied()
                && expected == character
            {
                stack.pop();
                bracket.partner = Some(structure.brackets[opening].offset);
                structure.brackets[opening].partner = Some(position);
            }
            structure.brackets.push(bracket);
        }
        position += character.len_utf8();
    }
    Some(())
}
