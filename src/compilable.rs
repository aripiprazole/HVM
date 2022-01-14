use std::collections::{BTreeMap, HashMap};
use crate::lambolt as lb;

// A Compilable is a Lambolt file ready for compilation.
// It includes data such as:
// - func_rules: sanitized rules grouped by function
// - id_to_name: converts ctr/cal ids to names
// - name_to_id: converts ctr/cal names to ids
// - ctr_is_cal: true if a ctr is a cal
// A sanitized rule has all its variables renamed to have unique names.
// Variables that are never used are renamed to "" (empty string).

pub struct SanitizedRule {
  pub rule: lb::Rule,
  pub uses: HashMap<String, u64>,
}

pub struct Compilable {
  pub func_rules: HashMap<String, Vec<SanitizedRule>>,
  pub id_to_name: HashMap<u64, String>,
  pub name_to_id: HashMap<String, u64>,
  pub ctr_is_cal: HashMap<String, bool>,
}

// Sanitize
// ========

// This big function sanitizes a rule. That is, it renames every variable in a
// rule, in order to make it unique. Moreover, it will also add a `.N` to the
// end of the name of each variable used in the right-hand side of the rule,
// where `N` stands for the number of times it was used. For example:
//   sanitize `(fn (cons head tail)) = (cons (pair head head) tail)`
//         ~> `(fn (cons x0   x1))   = (cons (pair x0.0 x0.1) x1.0)`
// It also returns the usage count of each variable.
pub fn sanitize_rule(rule: &lb::Rule) -> Result<SanitizedRule, String> {
  // Pass through the lhs of the function generating new names
  // for every variable found in the style described before with
  // the fresh function. Also checks if rule's left side is valid.
  // BTree is used here for determinism (HashMap does not maintain
  // order among executions)
  type NameTable = BTreeMap<String, String>;
  fn create_fresh(rule: &lb::Rule, fresh: &mut dyn FnMut() -> String) -> Result<NameTable, String> {
    let mut table = BTreeMap::new();

    let lhs = &rule.lhs;
    if let lb::Term::Ctr { ref name, ref args } = **lhs {
      for arg in args {
        match &**arg {
          lb::Term::Var { name, .. } => {
            table.insert(name.clone(), fresh());
          }
          lb::Term::Ctr { args, .. } => {
            for arg in args {
              if let lb::Term::Var { name } = &**arg {
                table.insert(name.clone(), fresh());
              } else {
                return Err("Invalid left-hand side".to_owned());
              }
            }
          }
          lb::Term::U32 { .. } => {}
          _ => {
            return Err("Invalid left-hand side".to_owned());
          }
        }
      }
    } else {
      return Err("Invalid left-hand side".to_owned());
    }

    Ok(table)
  }

  struct CtxSanitizeTerm<'a> {
    uses: &'a mut HashMap<String, u64>,
    fresh: &'a mut dyn FnMut() -> String,
  }

  // Sanitize one term, following the described in main function
  fn sanitize_term(
    term: &lb::Term,
    lhs: bool,
    tbl: &mut NameTable,
    ctx: &mut CtxSanitizeTerm,
  ) -> Result<Box<lb::Term>, String> {
    let term = match term {
      lb::Term::Var { name } => {
        if lhs {
          // create a var with the name generated before
          let name = tbl.get(name).unwrap_or(name);
          Box::new(lb::Term::Var { name: name.clone() })
        } else {
          // create a var with the name generated before
          // concatenated with '.{{times_used}}'
          let gen_name = tbl.get(name);
          if let Some(name) = gen_name {
            let used = {
              *ctx
                .uses
                .entry(name.clone())
                .and_modify(|x| *x += 1)
                .or_insert(1)
            };
            let name = format!("{}.{}", name, used - 1);
            Box::new(lb::Term::Var { name })
          } else {
            return Err(format!("Error: unbound variable {}.", name));
          }
        }
      }
      lb::Term::Dup {
        expr,
        body,
        nam0,
        nam1,
      } => {
        let new_nam0 = (ctx.fresh)();
        let new_nam1 = (ctx.fresh)();
        let expr = sanitize_term(expr, lhs, tbl, ctx)?;
        tbl.insert(nam0.clone(), new_nam0.clone());
        tbl.insert(nam1.clone(), new_nam1.clone());

        let body = sanitize_term(body, lhs, tbl, ctx)?;
        let nam0 = format!("{}.0", new_nam0.clone());
        let nam1 = format!("{}.0", new_nam1.clone());
        let term = lb::Term::Dup {
          nam0,
          nam1,
          expr,
          body,
        };
        Box::new(term)
      }
      lb::Term::Let { name, expr, body } => {
        let new_name = (ctx.fresh)();
        let expr = sanitize_term(expr, lhs, tbl, ctx)?;
        tbl.insert(name.clone(), new_name);

        let body = sanitize_term(body, lhs, tbl, ctx)?;
        let term = duplicator(&name, expr, body, ctx.uses);
        term
      }
      lb::Term::Lam { name, body } => {
        let new_name = (ctx.fresh)();
        tbl.insert(name.clone(), new_name.clone());
        let body = {
          let body = sanitize_term(body, lhs, tbl, ctx)?;
          let expr = Box::new(lb::Term::Var {
            name: new_name.clone(),
          });
          let body = duplicator(&name, expr, body, ctx.uses);
          body
        };
        let term = lb::Term::Lam {
          name: new_name.clone(),
          body,
        };
        Box::new(term)
      }
      lb::Term::App { func, argm } => {
        let func = sanitize_term(func, lhs, tbl, ctx)?;
        let argm = sanitize_term(argm, lhs, tbl, ctx)?;
        let term = lb::Term::App { func, argm };
        Box::new(term)
      }
      lb::Term::Ctr { name, args } => {
        let mut n_args = vec![];
        for arg in args {
          let arg = sanitize_term(arg, lhs, tbl, ctx)?;
          n_args.push(arg);
        }
        let term = lb::Term::Ctr {
          name: name.clone(),
          args: n_args,
        };
        Box::new(term)
      }
      lb::Term::Op2 { oper, val0, val1 } => {
        let val0 = sanitize_term(val0, lhs, tbl, ctx)?;
        let val1 = sanitize_term(val1, lhs, tbl, ctx)?;
        let term = lb::Term::Op2 {
          oper: *oper,
          val0,
          val1,
        };
        Box::new(term)
      }
      lb::Term::U32 { numb } => {
        let term = lb::Term::U32 { numb: *numb };
        Box::new(term)
      }
    };

    Ok(term)
  }

  // Duplicates all variables that are used more than once.
  // The process is done generating auxiliary variables and
  // applying dup on them.
  fn duplicator(
    name: &String,
    expr: Box<lb::Term>,
    body: Box<lb::Term>,
    uses: &HashMap<String, u64>,
  ) -> Box<lb::Term> {
    let amount = uses.get(name).map(|x| *x);
    // verify if variable is used more than once
    if amount > Some(1) {
      let amount = amount.unwrap(); // certainly is not None
      let duplicated_times = amount - 1; // times that name is duplicated
      let aux_qtt = amount - 2; // quantity of aux variables
      let mut vars = vec![];

      // generate name for duplicated variables
      for i in (aux_qtt..duplicated_times * 2).rev() {
        let i = i - aux_qtt; // moved to 0,1,..
        let key = format!("{}.{}", name, i);
        vars.push(key);
      }

      // generate name for aux variables
      for i in (0..aux_qtt).rev() {
        let key = format!("c.{}", i);
        vars.push(key);
      }

      // use aux variables to duplicate the variable
      let dup = lb::Term::Dup {
        nam0: vars.pop().unwrap(),
        nam1: vars.pop().unwrap(),
        expr,
        body: duplicator_go(1, duplicated_times, body, &mut vars),
      };

      Box::new(dup)
    } else {
      // if not used more than once just make a let then
      let term = lb::Term::Let {
        name: format!("{}.0", name),
        expr,
        body,
      };
      Box::new(term)
    }
  }

  // Recursive aux function to duplicate one varible
  // an amount of times
  fn duplicator_go(
    i: u64,
    duplicated_times: u64,
    body: Box<lb::Term>,
    vars: &mut Vec<String>,
  ) -> Box<lb::Term> {
    if i == duplicated_times {
      body
    } else {
      let nam0 = vars.pop().unwrap();
      let nam1 = vars.pop().unwrap();
      let exp0 = Box::new(lb::Term::Var {
        name: format!("c.{}", i - 1),
      });
      Box::new(lb::Term::Dup {
        nam0,
        nam1,
        expr: exp0,
        body: duplicator_go(i + 1, duplicated_times, body, vars),
      })
    }
  }

  let mut size = 0;
  let mut uses: HashMap<String, u64> = HashMap::new();

  // creates a new name for a variable
  // the first will receive x0, second x1, ...
  let mut fresh = || {
    let key = format!("x{}", size);
    size += 1;
    key
  };

  // generate table containing the new_names following
  // pattern described before
  let table = create_fresh(rule, &mut fresh)?;

  // create context for sanitize_term
  let mut ctx = CtxSanitizeTerm {
    uses: &mut uses,
    fresh: &mut fresh,
  };

  // sanitize left side
  let lhs = sanitize_term(&rule.lhs, true, &mut table.clone(), &mut ctx)?;
  // sanitize right side
  let mut rhs = sanitize_term(&rule.rhs, false, &mut table.clone(), &mut ctx)?;

  // duplicate right side variables that are used more than once
  for (key, value) in table {
    let expr = Box::new(lb::Term::Var {
      name: value.clone(),
    });
    rhs = duplicator(&value, expr, rhs, &mut uses);
  }

  // forms the new rules
  let rule = lb::Rule { lhs, rhs };
  Ok(SanitizedRule { rule, uses })
}

// Compilable
// ==========

pub fn gen_compilable(file: &lb::File) -> Compilable {
  // Generates a name table for a whole program. That table links constructor
  // names (such as `cons` and `succ`) to small ids (such as `0` and `1`).
  pub type NameToId = HashMap<String, u64>;
  pub type IdToName = HashMap<u64, String>;
  pub fn gen_name_to_id(rules: &Vec<lb::Rule>) -> NameToId {
    fn find_ctrs(term: &lb::Term, table: &mut NameToId, fresh: &mut u64) {
      match term {
        lb::Term::Dup { expr, body, .. } => {
          find_ctrs(expr, table, fresh);
          find_ctrs(body, table, fresh);
        }
        lb::Term::Let { expr, body, .. } => {
          find_ctrs(expr, table, fresh);
          find_ctrs(body, table, fresh);
        }
        lb::Term::Lam { body, .. } => {
          find_ctrs(body, table, fresh);
        }
        lb::Term::App { func, argm, .. } => {
          find_ctrs(func, table, fresh);
          find_ctrs(argm, table, fresh);
        }
        lb::Term::Op2 { val0, val1, .. } => {
          find_ctrs(val0, table, fresh);
          find_ctrs(val1, table, fresh);
        }
        lb::Term::Ctr { name, args } => {
          let id = table.get(name);
          if id.is_none() {
            let first_char = name.chars().next();
            if let Some(c) = first_char {
              if c == '.' {
                let id = &name[1..].parse::<u64>();
                if let Ok(id) = id {
                  table.insert(name.clone(), *id);
                }
              } else {
                table.insert(name.clone(), *fresh);
                *fresh += 1;
              }
            }
            for arg in args {
              find_ctrs(arg, table, fresh);
            }
          }
        }
        _ => (),
      }
    }
    let mut table = HashMap::new();
    let mut fresh = 0;
    for rule in rules {
      find_ctrs(&rule.lhs, &mut table, &mut fresh);
      find_ctrs(&rule.rhs, &mut table, &mut fresh);
    }
    table
  }
  pub fn invert(name_to_id: &NameToId) -> IdToName {
    let mut id_to_name: IdToName = HashMap::new();
    for (name, id) in name_to_id {
      id_to_name.insert(*id, name.clone());
    }
    return id_to_name;
  }

  // Finds constructors that are used as functions.
  pub type IsFunctionTable = HashMap<String, bool>;
  pub fn gen_ctr_is_cal(rules: &Vec<lb::Rule>) -> IsFunctionTable {
    let mut is_call: IsFunctionTable = HashMap::new();
    for rule in rules {
      let term = &rule.lhs;
      if let lb::Term::Ctr { ref name, .. } = **term {
        // FIXME: this looks wrong, will check later
        is_call.insert(name.clone(), true);
      }
    }
    is_call
  }

  // Groups rules by name. For example:
  //   (add (succ a) (succ b)) = (succ (succ (add a b)))
  //   (add (succ a) (zero)  ) = (succ a)
  //   (add (zero)   (succ b)) = (succ b)
  //   (add (zero)   (zero)  ) = (zero)
  // This is a group of 4 rules starting with the "add" name.
  pub type FuncRules = HashMap<String, Vec<SanitizedRule>>;
  pub fn gen_func_rules(rules: &Vec<lb::Rule>) -> FuncRules {
    let mut groups: FuncRules = HashMap::new();
    for rule in rules {
      if let lb::Term::Ctr { ref name, ref args } = *rule.lhs {
        let group = groups.get_mut(name);
        let sanit = sanitize_rule(&rule).unwrap();
        match group {
          None => {
            groups.insert(name.clone(), Vec::from([sanit]));
          }
          Some(group) => {
            group.push(sanit);
          }
        }
      }
    }
    groups
  }

  let func_rules = gen_func_rules(&file.rules);
  let name_to_id = gen_name_to_id(&file.rules);
  let id_to_name = invert(&name_to_id);
  let ctr_is_cal = gen_ctr_is_cal(&file.rules);
  return Compilable {
    func_rules,
    name_to_id,
    id_to_name,
    ctr_is_cal,
  };
}
