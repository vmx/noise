
use std::str;
use std::collections::HashMap;
use std::iter::Iterator;
use std::usize;

use error::Error;
use key_builder::KeyBuilder;
use stems::Stems;
use json_value::JsonValue;
use query::{Sort, Returnable, RetValue, RetObject, RetArray, RetLiteral, AggregateFun, SortInfo};
use filters::{QueryRuntimeFilter, ExactMatchFilter, StemmedWordFilter, StemmedWordPosFilter,
              StemmedPhraseFilter, DistanceFilter, AndFilter, OrFilter};


// TODO vmx 2016-11-02: Make it import "rocksdb" properly instead of needing to import the individual tihngs
use rocksdb::{IteratorMode, Snapshot};


pub struct Parser<'a> {
    query: String,
    offset: usize,
    kb: KeyBuilder,
    pub snapshot: Snapshot<'a>,
}

impl<'a> Parser<'a> {
    pub fn new(query: String, snapshot: Snapshot<'a>) -> Parser<'a> {
        Parser {
            query: query,
            offset: 0,
            kb: KeyBuilder::new(),
            snapshot: snapshot,
        }
    }

    fn ws(&mut self) {
        for char in self.query[self.offset..].chars() {
            if !char.is_whitespace() {
                break;
            }
            self.offset += char.len_utf8();
        }
    }

    fn consume(&mut self, token: &str) -> bool {
        if self.could_consume(token) {
            self.offset += token.len();
            self.ws();
            true
        } else {
            false
        }
    }


    fn must_consume(&mut self, token: &str) -> Result<(), Error>  {
        if self.could_consume(token) {
            self.offset += token.len();
            self.ws();
            Ok(())
        } else {
            Err(Error::Parse(format!("Expected '{}' at character {}, found {}.",
                                     token, self.offset,
                                     &self.query[self.offset..self.offset+1])))
        }
    }

    fn could_consume(&self, token: &str) -> bool {
        self.query[self.offset..].starts_with(token)
    }

    fn consume_key(&mut self) -> Result<Option<String>, Error> {
        if let Some(key) = self.consume_field() {
            Ok(Some(key))
        } else if let Some(key) = try!(self.consume_string_literal()) {
            Ok(Some(key))
        } else {
            Ok(None)
        }
    }

    fn consume_field(&mut self) -> Option<String> {
        let mut result = String::new();
        {
            let mut chars = self.query[self.offset..].chars();
            if let Some(c) = chars.next() {
                // first char cannot be numeric 
                if c.is_alphabetic() || '_' == c || '$' == c {
                    result.push(c);
                    for c in chars {
                        if c.is_alphanumeric() || '_' == c || '$' == c {
                            result.push(c);
                        } else {
                            break;
                        }
                    }
                }
            }
        } 
        if result.len() > 0 {
            self.offset += result.len();
            self.ws();
            Some(result)
        } else {
            None
        }
    }

    fn consume_integer(&mut self) -> Result<Option<i64>, Error> {
        let mut result = String::new();
        for char in self.query[self.offset..].chars() {
            if char >= '0' && char <= '9' {
                result.push(char);
            } else {
                break;
            }
        }
        if !result.is_empty() {
            self.offset += result.len();
            self.ws();
            Ok(Some(try!(result.parse())))
        } else {
            Ok(None)
        }
    }

    fn consume_default(&mut self) -> Result<Option<JsonValue>, Error> {
        if self.consume("default") {
            try!(self.must_consume("="));
            if let Some(json) = try!(self.json()) {
                Ok(Some(json))
            } else {
                Err(Error::Parse("Expected json value for default".to_string()))
            }
        } else {
            Ok(None)
        }
    }
    
    fn consume_aggregate(&mut self) -> Result<Option<(AggregateFun, 
                                                      KeyBuilder,
                                                      JsonValue)>, Error> {
        let offset = self.offset;
        let mut aggregate_fun = if self.consume("group") {
            AggregateFun::GroupAsc
        } else if self.consume("sum") {
            AggregateFun::Sum
        } else if self.consume("max") {
            AggregateFun::Max
        } else if self.consume("min") {
            AggregateFun::Min
        } else if self.consume("list") {
            AggregateFun::List
        } else if self.consume("concat") {
            AggregateFun::Concat
        } else if self.consume("avg") {
            AggregateFun::Avg
        } else if self.consume("count") {
            AggregateFun::Count
        } else {
            return Ok(None)
        };

        if self.consume("(") {
            if aggregate_fun == AggregateFun::Count {
                try!(self.must_consume(")"));
                Ok(Some((aggregate_fun, KeyBuilder::new(), JsonValue::Null)))
            } else if aggregate_fun == AggregateFun::Concat {
                if let Some(kb) = try!(self.consume_keypath()) {
                    let json = if self.consume("sep") {
                        try!(self.must_consume("="));
                        JsonValue::String(try!(self.must_consume_string_literal()))
                    } else {
                        JsonValue::String(",".to_string())
                    };
                    try!(self.must_consume(")"));
                    Ok(Some((aggregate_fun, kb, json)))
                } else {
                    Err(Error::Parse("Expected keypath or bind variable".to_string()))
                }
            } else if let Some(kb) = try!(self.consume_keypath()) {
                if self.consume("order") {
                    try!(self.must_consume("="));
                    if self.consume("asc") {
                        aggregate_fun = AggregateFun::GroupAsc;
                    } else if self.consume("desc") {
                        aggregate_fun = AggregateFun::GroupDesc;
                    } else {
                        return Err(Error::Parse("Expected asc or desc".to_string()));
                    }
                }
                try!(self.must_consume(")"));

                Ok(Some((aggregate_fun, kb, JsonValue::Null)))
            } else {
                Err(Error::Parse("Expected keypath or bind variable".to_string()))
            }
        } else {
            // this consumed word above might be a Bind var. Unconsume and return nothing.
            self.offset = offset;
            Ok(None)
        }
    }

    fn consume_keypath(&mut self) -> Result<Option<KeyBuilder>, Error> {
        let key: String = if self.consume(".") {
            if self.consume("[") {
                let key = try!(self.must_consume_string_literal());
                try!(self.must_consume("]"));
                key
            } else {
                 if let Some(key) = self.consume_field() {
                    key
                } else {
                    self.ws();
                    // this means return the whole document
                    return Ok(Some(KeyBuilder::new()));
                }
            }
        } else {
            return Ok(None);
        };

        let mut kb = KeyBuilder::new();
        kb.push_object_key(&key);
        loop {
            if self.consume("[") {
                if let Some(index) = try!(self.consume_integer()) {
                    kb.push_array_index(index as u64);
                } else {
                    return Err(Error::Parse("Expected array index integer.".to_string()));
                }
                try!(self.must_consume("]"));
            } else if self.consume(".") {
                if let Some(key) = self.consume_field() {
                    kb.push_object_key(&key);
                } else {
                    return Err(Error::Parse("Expected object key.".to_string()));
                }
            } else {
                break;
            }
        }
        self.ws();
        Ok(Some(kb))
    }

    fn consume_number(&mut self) -> Result<Option<f64>, Error> {
        // Yes this parsing code is hideously verbose. But it conforms exactly to the json spec
        // and uses the rust f64 parser, which can't tell us how many characters it used or needs.

        // At the end it then uses the std rust String::parse<f64>() method to parse and return
        // the f64 value and advance the self.offset. The rust method is a super set of the
        // allowable json syntax, so it will parse any valid json floating point number. It might
        // return an error if the number is out of bounds.
        let mut result = String::new();
        'outer: loop {
            // this loop isn't a loop, it's just there to scope the self borrow
            // and then jump to the end to do another borrow (self.ws())
            let mut chars = self.query[self.offset..].chars();
            let mut c = if let Some(c) = chars.next() {
                c
            } else {
                return Ok(None);
            };

            // parse the sign
            c = if c == '-' {
                result.push('-');
                if let Some(c) = chars.next() { c } else {return Ok(None); }
            } else {
                c
            };

            // parse the first digit
            let mut leading_zero = false;
            c = if c == '0' {
                result.push('0');
                leading_zero = true;
                if let Some(c) = chars.next() { c } else {return Ok(None); }
            } else if c >= '1' && c <= '9' {
                result.push(c);
                if let Some(c) = chars.next() { c } else {return Ok(None); }
            } else if result.is_empty() {
                // no sign or digits found. not a number
                return Ok(None);
            } else {
                return Err(Error::Parse("Expected digits after sign (-).".to_string()));
            };

            // parse remaning significant digits
            if !leading_zero {
                // no more digits allowed if first digit is zero
                loop {
                    c = if c >= '0' && c <= '9' {
                        result.push(c);
                        if let Some(c) = chars.next() {
                            c
                        } else {
                            break 'outer;
                        }
                    } else {
                        break;
                    };
                }
            }

            // parse decimal
            c = if c == '.' {
                result.push(c);
                if let Some(c) = chars.next() {
                    c
                } else {
                    return Err(Error::Parse("Expected digits after decimal point.".to_string()));
                }
            } else {
                break 'outer;
            };

            // parse mantissa
            let mut found_mantissa = false;
            loop {
                c = if c >= '0' && c <= '9' {
                    result.push(c);
                    found_mantissa = true;

                    if let Some(c) = chars.next() {
                        c
                    } else {
                        break 'outer;
                    }
                } else {
                    if found_mantissa {
                        break;
                    }
                    return Err(Error::Parse("Expected digits after decimal point.".to_string()));
                };
            }

            // parse exponent symbol
            c = if c == 'e' || c == 'E' {
                result.push(c);
                if let Some(c) = chars.next() {
                    c
                } else {
                    return Err(Error::Parse("Expected exponent after e.".to_string()));
                }
            } else {
                break 'outer;
            };
            
            // parse exponent sign
            c = if c == '+' || c == '-' {
                result.push(c);
                if let Some(c) = chars.next() {
                    c
                } else {
                    return Err(Error::Parse("Expected exponent after e.".to_string()));
                }
            } else {
                c
            };

            // parse exponent digits
            let mut found_exponent = false;
            loop {
                c = if c >= '0' && c <= '9' {
                    result.push(c);
                    found_exponent = true;
                    if let Some(c) = chars.next() {
                        c
                    } else {
                        break 'outer;
                    }
                } else {
                    if found_exponent {
                        break 'outer;
                    }
                    return Err(Error::Parse("Expected exponent after e.".to_string()));
                }
            }
        }

        self.offset += result.len();
        self.ws();
        Ok(Some(try!(result.parse())))
    }


    fn must_consume_string_literal(&mut self) -> Result<String, Error> {
        if let Some(string) = try!(self.consume_string_literal()) {
            Ok(string)
        } else {
            Err(Error::Parse("Expected string literal.".to_string()))
        }
    }

    fn consume_string_literal(&mut self) -> Result<Option<String>, Error> {
        let mut lit = String::new();
        if !self.could_consume("\"") {
            return Ok(None);
        }
        // can't consume("\"") the leading quote because it will also skip leading whitespace
        // inside the string literal
        self.offset += 1;
        {
        let mut chars = self.query[self.offset..].chars();
        'outer: loop {
            let char = if let Some(char) = chars.next() {
                char
            } else {
                break;
            };
            if char == '\\' {
                self.offset += 1;

                let char = if let Some(char) = chars.next() {
                    char
                } else {
                    break;
                };
                match char {
                    '\\' | '"' | '/' => lit.push(char),
                    'n' => lit.push('\n'),
                    'b' => lit.push('\x08'),
                    'r' => lit.push('\r'),
                    'f' => lit.push('\x0C'),
                    't' => lit.push('\t'),
                    'v' => lit.push('\x0B'),
                    'u' => {
                        let mut n = 0;
                        for _i in 0..4 {
                            let char = if let Some(char) = chars.next() {
                                char
                            } else {
                                break 'outer;
                            };
                            n = match char {
                                c @ '0' ... '9' => n * 16 + ((c as u16) - ('0' as u16)),
                                c @ 'a' ... 'f' => n * 16 + (10 + (c as u16) - ('a' as u16)),
                                c @ 'A' ... 'F' => n * 16 + (10 + (c as u16) - ('A' as u16)),
                                _ => return Err(Error::Parse(format!(
                                        "Invalid hexidecimal escape: {}", char))),
                            };
                            
                        }
                        self.offset += 3; // 3 because 1 is always added after the match below        
                    },
                    _ => return Err(Error::Parse(format!("Unknown character escape: {}",
                                                            char))),
                };
                self.offset += 1;
            } else {
                if char == '"' {
                    break;
                } else {
                    lit.push(char);
                    self.offset += char.len_utf8();
                }
            }
        }
        }
        try!(self.must_consume("\""));
        Ok(Some(lit))
    }

/*

find
	= "find" ws object ws

object
	= "{" ws obool ws "}" ws (("&&" / "||")  ws object)?
   / parens

parens
	= "(" ws object ws ")"

obool
   = ws ocompare ws (('&&' / ',' / '||') ws obool)?
   
ocompare
   = oparens
   / key ws ":" ws (oparens / compare)
   
oparens
   = '(' ws obool ws ')' ws
   / array
   / object

compare
   = ("==" / "~=" / "~" digits "=" ) ws string ws

abool
	= ws acompare ws (('&&'/ ',' / '||') ws abool)?
    
acompare
   = aparens
   / compare

aparens
   = '(' ws abool ')' ws
   / array
   / object

array
   = '[' ws abool ']' ws

key
   = field / string

field
	= [a-z_$]i [a-z_$0-9]i*

string
   = '"' ('\\\\' / '\\' [\"tfvrnb] / [^\\\"])* '"' ws

digits
    = [0-9]+

ws
 = [ \t\n\r]*

ws1
 = [ \t\n\r]+
*/


    fn find<'b>(&'b mut self) -> Result<Box<QueryRuntimeFilter + 'a>, Error> {
        if !self.consume("find") {
            return Err(Error::Parse("Missing 'find' keyword".to_string()));
        }
        self.object()
    }

    fn object<'b>(&'b mut self) -> Result<Box<QueryRuntimeFilter + 'a>, Error> {
        if self.consume("{") {
            let left = try!(self.obool());
            try!(self.must_consume("}"));
            
            if self.consume("&&") {
                let right = try!(self.object());
                Ok(Box::new(AndFilter::new(vec![left, right], self.kb.arraypath_len())))

            } else if self.consume("||") {
                let right = try!(self.object());
                Ok(Box::new(OrFilter::new(left, right, self.kb.arraypath_len())))
            } else {
                Ok(left)
            }
        } else {
             self.parens()
        }
    }

    fn parens<'b>(&'b mut self) -> Result<Box<QueryRuntimeFilter + 'a>, Error> {
        try!(self.must_consume("("));
        let filter = try!(self.object());
        try!(self.must_consume(")"));
        Ok(filter)
    }

    fn obool<'b>(&'b mut self) -> Result<Box<QueryRuntimeFilter + 'a>, Error> {
        let mut filter = try!(self.ocompare());
        loop {
            filter = if self.consume("&&") || self.consume(",") {
                let right = try!(self.obool());
                Box::new(AndFilter::new(vec![filter, right], self.kb.arraypath_len()))
            } else if self.consume("||") {
                let right = try!(self.obool());
                Box::new(OrFilter::new(filter, right, self.kb.arraypath_len()))
            } else {
                break;
            }
        }
        Ok(filter)
    }

    fn ocompare<'b>(&'b mut self) -> Result<Box<QueryRuntimeFilter + 'a>, Error> {
        if let Some(filter) = try!(self.oparens()) {
            Ok(filter)
        } else if let Some(field) = try!(self.consume_key()) {
            self.kb.push_object_key(&field);
            try!(self.must_consume(":"));
            if let Some(filter) = try!(self.oparens()) {
                self.kb.pop_object_key();
                Ok(filter)
            } else {
                let filter = try!(self.compare());
                self.kb.pop_object_key();
                Ok(filter)
            }
        } else {
            Err(Error::Parse("Expected object key or '('".to_string()))
        }
    }

    fn oparens<'b>(&'b mut self) -> Result<Option<Box<QueryRuntimeFilter + 'a>>, Error> {
        if self.consume("(") {
            let f = try!(self.obool());
            try!(self.must_consume(")"));
            Ok(Some(f))
        } else if self.could_consume("[") {
            Ok(Some(try!(self.array())))
        } else if self.could_consume("{") {
            Ok(Some(try!(self.object())))
        } else {
            Ok(None)
        }
    }

    fn compare<'b>(&'b mut self) -> Result<Box<QueryRuntimeFilter + 'a>, Error> {
        if self.consume("==") {
            let literal = try!(self.must_consume_string_literal());
            let stems = Stems::new(&literal);
            let mut filters: Vec<Box<QueryRuntimeFilter + 'a>> = Vec::new();
            for stem in stems {
                let iter = self.snapshot.iterator(IteratorMode::Start);
                let filter = Box::new(ExactMatchFilter::new(
                    iter, &stem, &self.kb));
                filters.push(filter);
            }
            match filters.len() {
                0 => panic!("Cannot create a ExactMatchFilter"),
                1 => Ok(filters.pop().unwrap()),
                _ => Ok(Box::new(AndFilter::new(filters, self.kb.arraypath_len()))),
            }
        } else if self.consume("~=") {
            // regular search
            let literal = try!(self.must_consume_string_literal());
            let stems = Stems::new(&literal);
            let stemmed_words: Vec<String> = stems.map(|stem| stem.stemmed).collect();

            match stemmed_words.len() {
                0 => panic!("Cannot create a StemmedWordFilter"),
                1 => {
                    let iter = self.snapshot.iterator(IteratorMode::Start);
                    Ok(Box::new(StemmedWordFilter::new(iter, &stemmed_words[0], &self.kb)))
                },
                _ => {
                    let mut filters: Vec<StemmedWordPosFilter> = Vec::new();
                    for stemmed_word in stemmed_words {
                        let iter = self.snapshot.iterator(IteratorMode::Start);
                        let filter = StemmedWordPosFilter::new(iter, &stemmed_word, &self.kb);
                        filters.push(filter);
                    }
                    Ok(Box::new(StemmedPhraseFilter::new(filters)))
                },
            }
        } else if self.consume("~") {
            let word_distance = match try!(self.consume_integer()) {
                Some(int) => int,
                None => {
                    return Err(Error::Parse("Expected integer for proximity search".to_string()));
                },
            };
            try!(self.must_consume("="));

            let literal = try!(self.must_consume_string_literal());
            let stems = Stems::new(&literal);
            let mut filters: Vec<StemmedWordPosFilter> = Vec::new();
            for stem in stems {
                let iter = self.snapshot.iterator(IteratorMode::Start);
                let filter = StemmedWordPosFilter::new(
                    iter, &stem.stemmed, &self.kb);
                filters.push(filter);
            }
            match filters.len() {
                0 => panic!("Cannot create a DistanceFilter"),
                _ => Ok(Box::new(DistanceFilter::new(filters, word_distance))),
            }
        } else {
            Err(Error::Parse("Expected comparison operator".to_string()))
        }
    }

    fn abool<'b>(&'b mut self) -> Result<Box<QueryRuntimeFilter + 'a>, Error> {
        let mut filter = try!(self.acompare());
        loop {
            filter = if self.consume("&&") || self.consume(",") {
                let right = try!(self.abool());
                Box::new(AndFilter::new(vec![filter, right], self.kb.arraypath_len()))
            } else if self.consume("||") {
                let right = try!(self.abool());
                Box::new(OrFilter::new(filter, right, self.kb.arraypath_len()))
            } else {
                break;
            }
        }
        Ok(filter)
    }

    fn acompare<'b>(&'b mut self) -> Result<Box<QueryRuntimeFilter + 'a>, Error> {
        if let Some(filter) = try!(self.aparens()) {
            Ok(filter)
        } else {
            self.compare()
        }
    }

    fn aparens<'b>(&'b mut self) -> Result<Option<Box<QueryRuntimeFilter + 'a>>, Error> {
        if self.consume("(") {
            let f = try!(self.abool());
            try!(self.must_consume(")"));
            Ok(Some(f))
        } else if self.could_consume("[") {
            Ok(Some(try!(self.array())))
        } else if self.could_consume("{") {
            Ok(Some(try!(self.object())))
        } else {
            Ok(None)
        }
    }

    fn array<'b>(&'b mut self) -> Result<Box<QueryRuntimeFilter + 'a>, Error> {
        if !self.consume("[") {
            return Err(Error::Parse("Expected '['".to_string()));
        }
        self.kb.push_array();
        let filter = try!(self.abool());
        self.kb.pop_array();
        try!(self.must_consume("]"));
        Ok(filter)
    }

    pub fn sort_clause(&mut self) -> Result<HashMap<String, SortInfo>, Error> {
        let mut sort_infos = HashMap::new();
        if self.consume("sort") {
            loop {
                if let Some(kb) = try!(self.consume_keypath()) {
                    // doing the search for source 2x so user can order
                    // anyway they like. Yes it's a hack, but it simple.
                    let mut sort = if self.consume("asc") {
                        Sort::Asc
                    } else if self.consume("desc") {
                        Sort::Desc
                    } else {
                        Sort::Asc
                    };

                    let default = if self.consume("default") {
                        try!(self.must_consume("="));
                        if let Some(json) = try!(self.json()) {
                            json
                        } else {
                            return Err(Error::Parse("Expected Json after default.".to_string()));
                        }
                    } else {
                        JsonValue::Null
                    };

                    sort = if self.consume("asc") {
                        Sort::Asc
                    } else if self.consume("desc") {
                        Sort::Desc
                    } else {
                        sort
                    };

                    sort_infos.insert(kb.value_key(0), SortInfo{kb:kb,
                                                                sort:sort,
                                                                default:default});
                    if !self.consume(",") {
                        break;
                    }
                }
            }
            if sort_infos.is_empty() {
                return Err(Error::Parse("Expected field path in sort expression.".to_string()));
            }
        }
        Ok(sort_infos)
    }

    pub fn return_clause(&mut self) -> Result<Box<Returnable>, Error> {
        if self.consume("return") {
            if let Some(ret_value) = try!(self.ret_value()) {
                Ok(ret_value)
            } else {
                Err(Error::Parse("Expected key, object or array to return.".to_string()))
            }
        } else {
            let mut kb = KeyBuilder::new();
            kb.push_object_key("_id");
            Ok(Box::new(RetValue{kb: kb, ag:None, default: JsonValue::Null, sort: None}))
        }
    }

    fn ret_object(&mut self) -> Result<Box<Returnable>, Error> {
        try!(self.must_consume("{"));
        let mut fields: Vec<(String, Box<Returnable>)> = Vec::new();
        loop {
            if let Some(field) = try!(self.consume_key()) {
                try!(self.must_consume(":"));
                if let Some(ret_value) = try!(self.ret_value()) {
                    fields.push((field, ret_value));
                    if !self.consume(",") {
                        break;
                    }
                } else {
                    return Err(Error::Parse("Expected key to return.".to_string()));
                }
            } else {
                break;
            }
        }
        
        try!(self.must_consume("}"));
        Ok(Box::new(RetObject{fields: fields}))
    }

    fn ret_array(&mut self) -> Result<Box<Returnable>, Error> {
        try!(self.must_consume("["));
        let mut slots = Vec::new();
        loop {
            if let Some(ret_value) = try!(self.ret_value()) {
                slots.push(ret_value);
                if !self.consume(",") {
                    break;
                }
            } else {
                break;
            }
        }
        try!(self.must_consume("]"));
        Ok(Box::new(RetArray{slots: slots}))

    }

    fn ret_value(&mut self) -> Result<Option<Box<Returnable>>, Error> {
        if let Some((ag, kb, json)) = try!(self.consume_aggregate()) {
            let default = if let Some(default) = try!(self.consume_default()) {
                default
            } else {
                JsonValue::Null
            };
            Ok(Some(Box::new(RetValue{kb: kb, ag: Some((ag, json)),
                                      default: default, sort:None})))
        }
        else if let Some(kb) = try!(self.consume_keypath()) {
            let default = if let Some(default) = try!(self.consume_default()) {
                default
            } else {
                JsonValue::Null
            };
            Ok(Some(Box::new(RetValue{kb: kb, ag: None, default: default, sort: None})))
        } else if self.could_consume("{") {
            Ok(Some(try!(self.ret_object())))
        } else if self.could_consume("[") {
            Ok(Some(try!(self.ret_array())))
        } else if let Some(string) = try!(self.consume_string_literal()) {
            Ok(Some(Box::new(RetLiteral{json: JsonValue::String(string)})))
        } else if let Some(num) = try!(self.consume_number()) {
            Ok(Some(Box::new(RetLiteral{json: JsonValue::Number(num)})))
        } else {
            if self.consume("true") {
                Ok(Some(Box::new(RetLiteral{json: JsonValue::True})))
            } else if self.consume("false") {
                Ok(Some(Box::new(RetLiteral{json: JsonValue::False})))
            } else if self.consume("null") {
                Ok(Some(Box::new(RetLiteral{json: JsonValue::Null})))
            } else {
                Ok(None)
            }
        }
    }

    pub fn limit_clause(&mut self) -> Result<usize, Error> {
        if self.consume("limit") {
            if let Some(i) = try!(self.consume_integer()) {
                if i <= 0 {
                    return Err(Error::Parse("limit must be an integer greater than 0"
                                            .to_string()));
                }
                Ok(i as usize)
            } else {
                return Err(Error::Parse("limit expects an integer greater than 0"
                                            .to_string()));
            }
        } else {
            Ok(usize::MAX)
        }
    }

    fn json(&mut self) -> Result<Option<JsonValue>, Error> {
        if self.could_consume("{") {
            Ok(Some(try!(self.json_object())))
        } else if self.could_consume("[") {
            Ok(Some(try!(self.json_array())))
        } else if let Some(string) = try!(self.consume_string_literal()) {
            Ok(Some(JsonValue::String(string)))
        } else {
            if self.consume("true") {
                Ok(Some(JsonValue::True))
            } else if self.consume("false") {
                Ok(Some(JsonValue::False))
            } else if self.consume("null") {
                Ok(Some(JsonValue::Null))
            } else if let Some(num) = try!(self.consume_number()) {
                Ok(Some(JsonValue::Number(num)))
            } else {
                Ok(None)
            }
        }
    }

    fn json_object(&mut self) -> Result<JsonValue, Error> {
        try!(self.must_consume("{"));
        let mut object = Vec::new();
        if self.consume("}") {
            return Ok(JsonValue::Object(object));
        }
        loop {
            if let Some(field) = try!(self.consume_key()) {
                try!(self.must_consume(":"));
                if let Some(json) = try!(self.json()) {
                    object.push((field, json));
                    if !self.consume(",") {
                        break;
                    }
                } else {
                    return Err(Error::Parse("Invalid json found".to_string()));
                }
            } else {
                return Err(Error::Parse("Invalid json found".to_string()));
            }
        }
        try!(self.must_consume("}"));
        Ok(JsonValue::Object(object))
    }

    fn json_array(&mut self) -> Result<JsonValue, Error> {
        try!(self.must_consume("["));
        let mut array = Vec::new();
        if self.consume("]") {
            return Ok(JsonValue::Array(array));
        }
        loop {
            if let Some(json) = try!(self.json()) {
                array.push(json);
                if !self.consume(",") {
                    break;
                }
            } else {
                return Err(Error::Parse("Invalid json found".to_string()));
            }
        }
        try!(self.must_consume("]"));
        Ok(JsonValue::Array(array))
    }

    pub fn build_filter(&mut self) -> Result<Box<QueryRuntimeFilter + 'a>, Error> {
        self.ws();
        Ok(try!(self.find()))
    }
}

#[cfg(test)]
mod tests {

    use super::Parser;

    use index::{Index, OpenOptions};

    use rocksdb::Snapshot;
    
    #[test]
    fn test_whitespace() {
        let mut index = Index::new();
        index.open("target/tests/test_whitespace", Some(OpenOptions::Create)).unwrap();
        let rocks = &index.rocks.unwrap();
        let mut snapshot = Snapshot::new(rocks);

        let mut query = " \n \t test".to_string();
        let mut parser = Parser::new(query, snapshot);
        parser.ws();
        assert_eq!(parser.offset, 5);

        snapshot = Snapshot::new(rocks);
        query = "test".to_string();
        parser = Parser::new(query, snapshot);
        parser.ws();
        assert_eq!(parser.offset, 0);
    }

    #[test]
    fn test_must_consume_string_literal() {
        let mut index = Index::new();
        index.open("target/tests/test_must_consume_string_literal", Some(OpenOptions::Create)).unwrap();
        let rocks = &index.rocks.unwrap();
        let snapshot = Snapshot::new(rocks);

        let query = r#"" \n \t test""#.to_string();
        let mut parser = Parser::new(query, snapshot);
        assert_eq!(parser.must_consume_string_literal().unwrap(),  " \n \t test".to_string());
    }
}