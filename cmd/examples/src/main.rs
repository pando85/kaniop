mod group;
mod kanidm;
mod oauth2;
mod person;
mod yaml;

use schemars::gen::SchemaSettings;
use yaml::write_to_file;

fn main() {
    let kanidm = kanidm::example();
    let person = person::example(&kanidm);
    let group = group::example(&kanidm, &person);
    let oauth2 = oauth2::example();

    let settings = SchemaSettings::default().with(|s| {
        s.inline_subschemas = true;
    });
    let gen = settings.into_generator();
    write_to_file(&group, &group::schema(&gen), "examples/group.yaml").unwrap();
    write_to_file(&kanidm, &kanidm::schema(&gen), "examples/kanidm.yaml").unwrap();
    write_to_file(&oauth2, &oauth2::schema(&gen), "examples/oauth2.yaml").unwrap();
    write_to_file(&person, &person::schema(&gen), "examples/person.yaml").unwrap();
}
