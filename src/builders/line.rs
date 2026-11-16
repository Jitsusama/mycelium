//! A Nix machines file's lines, as Nix reads them: a builder a line, its
//! fields apart by spaces, `-` for one left as Nix's default, `#` to the
//! end of a line a comment, and `;` between builders as well as a line's
//! end. Only what is said of a builder is kept: its host, the systems it
//! builds, its jobs, its speed and its features. Its key's path, its host
//! key and the options in its address are never read past, so nothing
//! that opens a builder goes further than the file.

/// One builder as a line of a machines file says it.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// The host its address names, such as `kestrel.local`, as SSH is
    /// given it: an alias of SSH's configuration, or a name or address.
    pub host: String,
    /// The port its address names, where it names one.
    pub port: Option<u16>,
    /// The systems it builds for, such as `x86_64-linux`.
    pub systems: Vec<String>,
    /// How many builds it takes at once; Nix's default is one.
    pub jobs: u32,
    /// How much faster than one it builds, which Nix ranks builders by;
    /// Nix's default is one.
    pub speed: f64,
    /// The features it supports, such as `kvm`, the mandatory among them.
    pub supported: Vec<String>,
    /// The features a build must ask for to be sent to it.
    pub mandatory: Vec<String>,
}

/// The builders `text`, a machines file, names, in its order. A line
/// naming no host SSH would reach, such as Nix's own `local`, or one
/// whose numbers don't read, is passed over, as it builds nothing here.
#[must_use]
pub fn lines(text: &str) -> Vec<Line> {
    text.split(['\n', ';'])
        .filter_map(|line| line_of(line.split('#').next().unwrap_or_default()))
        .collect()
}

/// The builder `line` names, or none.
fn line_of(line: &str) -> Option<Line> {
    let mut fields = line.split_whitespace().map(|field| match field {
        "-" => "",
        said => said,
    });
    let (host, port) = host_of(fields.next()?)?;
    let systems = listed(fields.next());
    // The key's path is passed over, never kept.
    let _key = fields.next();
    let jobs = number(fields.next(), 1)?;
    let speed = number(fields.next(), 1.0)?;
    let mut supported = listed(fields.next());
    let mandatory = listed(fields.next());
    // A mandatory feature is one it supports, as Nix reads it.
    for feature in &mandatory {
        if !supported.contains(feature) {
            supported.push(feature.clone());
        }
    }
    Some(Line {
        host,
        port,
        systems,
        jobs,
        speed,
        supported,
        mandatory,
    })
}

/// The host and port of `address`, a store's address as Nix takes one,
/// such as `ssh-ng://builder@kestrel.local?compress=true` or a bare
/// `kestrel`; none for a store reached by no SSH.
fn host_of(address: &str) -> Option<(String, Option<u16>)> {
    let rest = match address.split_once("://") {
        Some(("ssh" | "ssh-ng", rest)) => rest,
        Some(_) => return None,
        None if address == "local" || address == "auto" || address == "daemon" => return None,
        None => address,
    };
    let authority = rest.split(['?', '/']).next().unwrap_or_default();
    let at = authority.rsplit('@').next().unwrap_or_default();
    let (host, port) = match at.strip_prefix('[') {
        Some(bracketed) => {
            let (host, after) = bracketed.split_once(']')?;
            (host, after.strip_prefix(':'))
        }
        None => match at.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (at, None),
        },
    };
    // A host is given to SSH as a word of its own, so one that reads as an
    // option is no host.
    if host.is_empty() || host.starts_with('-') {
        return None;
    }
    let port = port.map(str::parse).transpose().ok()?;
    Some((host.to_owned(), port))
}

/// A field of names apart by commas, none where it is left out.
fn listed(field: Option<&str>) -> Vec<String> {
    field
        .unwrap_or_default()
        .split(',')
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A field's number, `default` where it is left out; none where it says
/// something else.
fn number<T: std::str::FromStr>(field: Option<&str>, default: T) -> Option<T> {
    match field {
        None | Some("") => Some(default),
        Some(said) => said.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(host: &str, systems: &[&str], jobs: u32, speed: f64) -> Line {
        Line {
            host: host.to_owned(),
            port: None,
            systems: systems.iter().map(|&each| each.to_owned()).collect(),
            jobs,
            speed,
            supported: Vec::new(),
            mandatory: Vec::new(),
        }
    }

    fn named(names: &[&str]) -> Vec<String> {
        names.iter().map(|&each| each.to_owned()).collect()
    }

    #[test]
    fn a_line_says_its_host_systems_jobs_speed_and_features() {
        let text = "ssh-ng://builder@kestrel.local?compress=true x86_64-linux,aarch64-linux \
                    /etc/nix/kestrel_ed25519 4 2 kvm,big-parallel nixos-test c3NoLWVkMjU1MTk=\n";
        let mut kestrel = line("kestrel.local", &["x86_64-linux", "aarch64-linux"], 4, 2.0);
        kestrel.supported = named(&["kvm", "big-parallel", "nixos-test"]);
        kestrel.mandatory = named(&["nixos-test"]);
        assert_eq!(
            lines(text),
            vec![kestrel],
            "a mandatory feature is one it supports, and its key is kept by nobody"
        );
    }

    #[test]
    fn a_field_left_out_or_dashed_is_nixs_default() {
        assert_eq!(
            lines("ssh://skiff - - - - - -\nkestrel x86_64-linux"),
            vec![
                line("skiff", &[], 1, 1.0),
                line("kestrel", &["x86_64-linux"], 1, 1.0)
            ],
            "one job at a speed of one, and no systems or features said"
        );
    }

    #[test]
    fn comments_blank_lines_and_semicolons_part_builders_as_nix_reads_them() {
        let text =
            "# the standing builders\n\n  skiff x86_64-linux # the boat\nkestrel; ssh://[::1]:2222";
        let mut local = line("::1", &[], 1, 1.0);
        local.port = Some(2222);
        assert_eq!(
            lines(text),
            vec![
                line("skiff", &["x86_64-linux"], 1, 1.0),
                line("kestrel", &[], 1, 1.0),
                local,
            ],
        );
    }

    #[test]
    fn a_line_no_ssh_reaches_or_whose_numbers_dont_read_builds_nothing_here() {
        assert_eq!(
            lines(
                "local\nunix:///run/nix x86_64-linux\nskiff x86_64-linux - many\n\
                 ssh://@ x86_64-linux\nssh://-oProxyCommand=true x86_64-linux"
            ),
            Vec::new(),
        );
    }
}
