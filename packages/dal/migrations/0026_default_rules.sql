-- Default auto-classification rules.
--
-- Sure ships with no rules, so every imported transaction lands uncategorised and has
-- to be sorted by hand. These are the merchant and bank-wording patterns that carried a
-- real NZ transaction history: supermarkets, fuel, the statement markers banks use for
-- interest, fees and internal transfers, and the recurring national chains.
--
-- Both halves are conditional inserts, so this is a no-op on a database that already has
-- the category or a rule of the same name, and seeds them on a fresh one. The categories
-- have to come first: a rule stores `set_category_id`, and Sure creates categories at
-- runtime (provider enrichment's find_or_create, or the user), never in a migration — so
-- without them here every seeded rule would resolve its category to NULL and do nothing.
--
-- Names match the taxonomy Akahu enrichment produces, so a later import finds these by
-- name instead of creating a second copy.
--
-- Patterns are matched with contains(lower(description), ...). Tokens are kept short
-- enough to survive ASB's twelve-character memo split, which lands mid-word:
-- "PAK N SAVE A LBANY DR ALBANY", "AMAZON WEB S ERVICES". A token that straddles the
-- split ("amazon web services") would never match, so each one fits inside one segment.
-- zen-expression has no backslash escape inside a string literal, so a pattern holding an
-- apostrophe is double-quoted ("pak'nsave") — the same idiom scripts/seed.mjs uses.
--
-- Deliberately not here: rules keyed to a named person, employer or account number. Those
-- are specific to one person's statements and are rule 3 material (CLAUDE.md); they are
-- created in the app, not shipped.

-- ---- categories the rules below classify into ----------------------------------

INSERT INTO categories (name, parent_id, kind)
SELECT 'Appearance', NULL, 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Appearance');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Education', NULL, 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Education');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Food', NULL, 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Food');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Health', NULL, 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Health');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Household', NULL, 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Household');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Housing', NULL, 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Housing');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Lifestyle', NULL, 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Lifestyle');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Professional Services', NULL, 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Professional Services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Transport', NULL, 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Transport');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Utilities', NULL, 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Utilities');


INSERT INTO categories (name, parent_id, kind)
SELECT 'Air transport services', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Air transport services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Automotive parts and accessories', (SELECT id FROM categories WHERE name = 'Transport' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Automotive parts and accessories');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Automotive repair and servicing', (SELECT id FROM categories WHERE name = 'Transport' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Automotive repair and servicing');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Bakeries', (SELECT id FROM categories WHERE name = 'Food' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Bakeries');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Bank fees and charges', (SELECT id FROM categories WHERE name = 'Professional Services' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Bank fees and charges');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Bars, pubs, nightclubs', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Bars, pubs, nightclubs');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Bicycle stores, rentals, and repairs', (SELECT id FROM categories WHERE name = 'Transport' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Bicycle stores, rentals, and repairs');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Book stores', (SELECT id FROM categories WHERE name = 'Education' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Book stores');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Building supplies', (SELECT id FROM categories WHERE name = 'Household' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Building supplies');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Bus and shuttle transport services', (SELECT id FROM categories WHERE name = 'Transport' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Bus and shuttle transport services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Business software and cloud services', (SELECT id FROM categories WHERE name = 'Professional Services' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Business software and cloud services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Cafes and restaurants', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Cafes and restaurants');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Cash withdrawals and deposits', NULL, 'transfer'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Cash withdrawals and deposits');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Cinemas', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Cinemas');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Clothing stores', (SELECT id FROM categories WHERE name = 'Appearance' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Clothing stores');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Convenience stores', (SELECT id FROM categories WHERE name = 'Food' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Convenience stores');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Cosmetic, health spas, and relaxation massage services', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Cosmetic, health spas, and relaxation massage services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Digital gaming products and services', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Digital gaming products and services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Doctors and physicians', (SELECT id FROM categories WHERE name = 'Health' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Doctors and physicians');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Electricity services', (SELECT id FROM categories WHERE name = 'Utilities' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Electricity services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Electronic and appliance stores', (SELECT id FROM categories WHERE name = 'Household' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Electronic and appliance stores');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Events and tickets (not elsewhere classified)', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Events and tickets (not elsewhere classified)');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Fabric, sewing, knitting, and related supplies', (SELECT id FROM categories WHERE name = 'Appearance' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Fabric, sewing, knitting, and related supplies');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Fast food stores', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Fast food stores');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Financial asset brokers, exchanges, and managed funds', (SELECT id FROM categories WHERE name = 'Professional Services' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Financial asset brokers, exchanges, and managed funds');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Floral supplies and services', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Floral supplies and services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Fuel stations', (SELECT id FROM categories WHERE name = 'Transport' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Fuel stations');

INSERT INTO categories (name, parent_id, kind)
SELECT 'General retail stores', (SELECT id FROM categories WHERE name = 'Household' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'General retail stores');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Gift and souvenir stores', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Gift and souvenir stores');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Gyms, fitness, aquatic facilities, yoga, pilates', (SELECT id FROM categories WHERE name = 'Health' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Gyms, fitness, aquatic facilities, yoga, pilates');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Haircuts and treatments', (SELECT id FROM categories WHERE name = 'Appearance' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Haircuts and treatments');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Home furnishing and repair stores', (SELECT id FROM categories WHERE name = 'Household' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Home furnishing and repair stores');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Hotels, motels, and other temporary accommodation', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Hotels, motels, and other temporary accommodation');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Insurance', (SELECT id FROM categories WHERE name = 'Household' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Insurance');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Interest charged', NULL, 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Interest charged');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Interest earned', NULL, 'income'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Interest earned');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Internet services', (SELECT id FROM categories WHERE name = 'Utilities' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Internet services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Laundry and drycleaning', (SELECT id FROM categories WHERE name = 'Household' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Laundry and drycleaning');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Libraries', (SELECT id FROM categories WHERE name = 'Education' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Libraries');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Liquor stores', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Liquor stores');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Local government', (SELECT id FROM categories WHERE name = 'Housing' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Local government');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Meal kit stores', (SELECT id FROM categories WHERE name = 'Food' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Meal kit stores');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Media and entertainment streaming services', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Media and entertainment streaming services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Parking services', (SELECT id FROM categories WHERE name = 'Transport' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Parking services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Pets and related supplies, accommodation, and services', (SELECT id FROM categories WHERE name = 'Household' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Pets and related supplies, accommodation, and services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Pharmacies', (SELECT id FROM categories WHERE name = 'Health' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Pharmacies');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Secondhand and opportunity stores', (SELECT id FROM categories WHERE name = 'Appearance' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Secondhand and opportunity stores');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Shoe stores', (SELECT id FROM categories WHERE name = 'Appearance' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Shoe stores');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Specialty food stores', (SELECT id FROM categories WHERE name = 'Food' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Specialty food stores');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Sports equipment and supplies', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Sports equipment and supplies');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Stationery and office supplies', (SELECT id FROM categories WHERE name = 'Household' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Stationery and office supplies');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Student loan', NULL, 'transfer'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Student loan');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Supermarkets and grocery stores', (SELECT id FROM categories WHERE name = 'Food' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Supermarkets and grocery stores');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Tax payments', (SELECT id FROM categories WHERE name = 'Professional Services' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Tax payments');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Taxi, rideshare, and on-demand transport services', (SELECT id FROM categories WHERE name = 'Transport' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Taxi, rideshare, and on-demand transport services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Telecommunication services', (SELECT id FROM categories WHERE name = 'Utilities' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Telecommunication services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Transfer', NULL, 'transfer'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Transfer');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Transport services (not elsewhere classified)', (SELECT id FROM categories WHERE name = 'Transport' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Transport services (not elsewhere classified)');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Universities, professional schools, and other tertiary education', (SELECT id FROM categories WHERE name = 'Education' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Universities, professional schools, and other tertiary education');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Waste and recycling services', (SELECT id FROM categories WHERE name = 'Household' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Waste and recycling services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Water services', (SELECT id FROM categories WHERE name = 'Utilities' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Water services');

INSERT INTO categories (name, parent_id, kind)
SELECT 'Welfare and charity', (SELECT id FROM categories WHERE name = 'Lifestyle' AND parent_id IS NULL), 'expense'
WHERE NOT EXISTS (SELECT 1 FROM categories WHERE name = 'Welfare and charity');


-- ---- the rules -----------------------------------------------------------------

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Loan repayment interest → Interest charged',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''loan repayment'') and contains(lower(description), ''interest''))',
       (SELECT id FROM categories WHERE name = 'Interest charged'),
       0, 1, 0, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Loan repayment interest → Interest charged');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Loan repayment principal → Transfer',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''loan repayment'') and contains(lower(description), ''principal''))',
       (SELECT id FROM categories WHERE name = 'Transfer'),
       0, 1, 1, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Loan repayment principal → Transfer');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Resident withholding tax → Tax payments',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''ird:tax on'') or contains(lower(description), ''i.r.d.'') or contains(lower(description), ''acc levy'') or contains(lower(description), ''inland revenue''))',
       (SELECT id FROM categories WHERE name = 'Tax payments'),
       0, 1, 2, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Resident withholding tax → Tax payments');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Student loan drawdowns and deductions → Student loan',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''repayment deduction'') or contains(lower(description), ''living costs'') or contains(lower(description), ''compulsory course fee'') or contains(lower(description), ''course related costs''))',
       (SELECT id FROM categories WHERE name = 'Student loan'),
       0, 1, 3, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Student loan drawdowns and deductions → Student loan');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Bank fees and charges → Bank fees',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''offshoreservicemargins'') or contains(lower(description), ''card service fee'') or contains(lower(description), ''establishment fee'') or contains(lower(description), ''administration fee'') or contains(lower(description), ''monthly account fee'') or contains(lower(description), ''overdraft fee'') or contains(lower(description), ''dishonour fee'') or contains(lower(description), ''bank fee''))',
       (SELECT id FROM categories WHERE name = 'Bank fees and charges'),
       0, 1, 4, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Bank fees and charges → Bank fees');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Bank interest paid → Interest earned',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''cr.int'') or contains(lower(description), ''credit int'') or contains(lower(description), ''reward interest'') or contains(lower(description), ''interest earned''))',
       (SELECT id FROM categories WHERE name = 'Interest earned'),
       0, 1, 5, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Bank interest paid → Interest earned');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'ATM and branch cash → Cash',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''atm deposit'') or contains(lower(description), ''atm wd'') or contains(lower(description), ''atm withdrawal'') or contains(lower(description), ''withdrawal'') or contains(lower(description), ''cash deposit''))',
       (SELECT id FROM categories WHERE name = 'Cash withdrawals and deposits'),
       0, 1, 6, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'ATM and branch cash → Cash');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Electricity retailers → Electricity',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''frank energy'') or contains(lower(description), ''contact energy'') or contains(lower(description), ''genesis ener'') or contains(lower(description), ''mercury nz'') or contains(lower(description), ''meridian en'') or contains(lower(description), ''electric kiwi'') or contains(lower(description), ''flick electr'') or contains(lower(description), ''nova energy'') or contains(lower(description), ''powershop''))',
       (SELECT id FROM categories WHERE name = 'Electricity services'),
       0, 1, 7, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Electricity retailers → Electricity');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Watercare → Water services',
       'Default rule shipped with Sure.',
       'contains(lower(description), ''watercare'')',
       (SELECT id FROM categories WHERE name = 'Water services'),
       0, 1, 8, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Watercare → Water services');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Telcos → Telecommunications',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''skinny'') or contains(lower(description), ''one nz'') or contains(lower(description), ''vodafone'') or contains(lower(description), ''2degrees'') or contains(lower(description), ''spark nz'') or contains(lower(description), ''warehouse mobile''))',
       (SELECT id FROM categories WHERE name = 'Telecommunication services'),
       0, 1, 9, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Telcos → Telecommunications');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'ISPs → Internet services',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''slingshot'') or contains(lower(description), ''orcon'') or contains(lower(description), ''2talk'') or contains(lower(description), ''bigpipe'') or contains(lower(description), ''nowbroadband'') or contains(lower(description), ''quic broadband''))',
       (SELECT id FROM categories WHERE name = 'Internet services'),
       0, 1, 10, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'ISPs → Internet services');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Councils → Local government',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''auckland coun'') or contains(lower(description), ''christchurch city c'') or contains(lower(description), ''ccc rates'') or contains(lower(description), ''city council'') or contains(lower(description), ''district counc''))',
       (SELECT id FROM categories WHERE name = 'Local government'),
       0, 1, 11, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Councils → Local government');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Cloud and software services → Business software',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''amazon web'') or contains(lower(description), ''aws* amazon'') or contains(lower(description), ''cloudflare'') or contains(lower(description), ''digitalocean'') or contains(lower(description), ''microsoft*'') or contains(lower(description), ''chatgpt'') or contains(lower(description), ''anthropic'') or contains(lower(description), ''claude.ai'') or contains(lower(description), ''openai'') or contains(lower(description), ''github'') or contains(lower(description), ''google clou'') or contains(lower(description), ''atlassian'') or contains(lower(description), ''jetbrains'') or contains(lower(description), ''namecheap'') or contains(lower(description), ''godaddy'') or contains(lower(description), ''vercel'') or contains(lower(description), ''netlify'') or contains(lower(description), ''twilio'') or contains(lower(description), ''acuityinsigh'') or contains(lower(description), ''adobe'') or contains(lower(description), ''dropbox'') or contains(lower(description), ''notion labs'') or contains(lower(description), ''figma''))',
       (SELECT id FROM categories WHERE name = 'Business software and cloud services'),
       0, 1, 12, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Cloud and software services → Business software');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Streaming services → Media and entertainment streaming',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''netflix'') or contains(lower(description), ''spotify'') or contains(lower(description), ''disney'') or contains(lower(description), ''neon.co.nz'') or contains(lower(description), ''prime video'') or contains(lower(description), ''youtubeprem'') or contains(lower(description), ''apple.com/bi'') or contains(lower(description), ''apple new ze'') or contains(lower(description), ''audible'') or contains(lower(description), ''crunchyroll''))',
       (SELECT id FROM categories WHERE name = 'Media and entertainment streaming services'),
       0, 1, 13, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Streaming services → Media and entertainment streaming');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Games and game platforms → Digital gaming',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''steam'') or contains(lower(description), ''twitch'') or contains(lower(description), ''epic games'') or contains(lower(description), ''nintendo'') or contains(lower(description), ''playstation'') or contains(lower(description), ''xbox'') or contains(lower(description), ''humble bundl'') or contains(lower(description), ''gog.com'') or contains(lower(description), ''mojang'') or contains(lower(description), ''roblox'') or contains(lower(description), ''discord''))',
       (SELECT id FROM categories WHERE name = 'Digital gaming products and services'),
       0, 1, 14, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Games and game platforms → Digital gaming');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Fuel — supermarket fuel stops → Fuel stations',
       'Default rule shipped with Sure.',
       '((contains(lower(description), ''save'') and contains(lower(description), ''fuel'')) or (contains(lower(description), ''save'') and contains(lower(description), ''f uel'')) or (contains(lower(description), ''new world'') and contains(lower(description), ''fuel'')))',
       (SELECT id FROM categories WHERE name = 'Fuel stations'),
       0, 1, 15, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Fuel — supermarket fuel stops → Fuel stations');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Fuel stations → Fuel',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''mobil'') or contains(lower(description), ''bp connect'') or contains(lower(description), ''bp 2go'') or contains(lower(description), ''bp oil'') or contains(lower(description), ''caltex'') or contains(lower(description), ''gull '') or contains(lower(description), ''npd '') or contains(lower(description), ''waitomo'') or contains(lower(description), ''kiwi fuels'') or contains(lower(description), ''apl fuel'') or contains(lower(description), ''challenge te'') or contains(lower(description), ''g.a.s.'') or contains(lower(description), ''allied fuel'') or contains(lower(description), '' z '') or contains(lower(description), '' zed '') or startsWith(lower(description), ''z '') or startsWith(lower(description), ''zed ''))',
       (SELECT id FROM categories WHERE name = 'Fuel stations'),
       0, 1, 16, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Fuel stations → Fuel');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Rideshare and micromobility → Taxi, rideshare',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''uber* trip'') or contains(lower(description), ''uber *trip'') or contains(lower(description), ''ubr* pending'') or contains(lower(description), ''ubr*'') or contains(lower(description), ''zoomy'') or contains(lower(description), ''ola '') or contains(lower(description), ''lioncity esc'') or contains(lower(description), ''beam mobilit'') or contains(lower(description), ''lime ride'') or contains(lower(description), ''co-op taxi'') or contains(lower(description), ''corporate ca'') or contains(lower(description), ''blue bubble''))',
       (SELECT id FROM categories WHERE name = 'Taxi, rideshare, and on-demand transport services'),
       0, 1, 17, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Rideshare and micromobility → Taxi, rideshare');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Public transport → Bus and shuttle',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''at hop'') or contains(lower(description), ''at hop auckl'') or contains(lower(description), ''metrocard'') or contains(lower(description), ''metro card'') or contains(lower(description), ''snapper'') or contains(lower(description), ''bee card'') or contains(lower(description), ''transportfor'') or contains(lower(description), ''auckland tra''))',
       (SELECT id FROM categories WHERE name = 'Bus and shuttle transport services'),
       0, 1, 18, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Public transport → Bus and shuttle');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Parking → Parking services',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''parking'') or contains(lower(description), ''parkmate'') or contains(lower(description), ''orikan'') or contains(lower(description), ''wilson '') or contains(lower(description), ''carpark'') or contains(lower(description), ''car park'') or contains(lower(description), ''secure park'') or contains(lower(description), ''care park'') or contains(lower(description), ''tournament p''))',
       (SELECT id FROM categories WHERE name = 'Parking services'),
       0, 1, 19, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Parking → Parking services');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Ferries, road user charges → Transport services',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''nz transport'') or contains(lower(description), ''bluebridge'') or contains(lower(description), ''straitnz'') or contains(lower(description), ''interislande'') or contains(lower(description), ''carjam'') or contains(lower(description), ''waka kotahi'') or contains(lower(description), ''nzta''))',
       (SELECT id FROM categories WHERE name = 'Transport services (not elsewhere classified)'),
       0, 1, 20, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Ferries, road user charges → Transport services');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Automotive repair → Automotive repair and servicing',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''automot'') or contains(lower(description), ''hillauto'') or contains(lower(description), ''vtnz'') or contains(lower(description), ''albany toyot'') or contains(lower(description), ''auto electri'') or contains(lower(description), ''tyre''))',
       (SELECT id FROM categories WHERE name = 'Automotive repair and servicing'),
       0, 1, 21, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Automotive repair → Automotive repair and servicing');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Automotive parts → Automotive parts',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''repco'') or contains(lower(description), ''supercheap''))',
       (SELECT id FROM categories WHERE name = 'Automotive parts and accessories'),
       0, 1, 22, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Automotive parts → Automotive parts');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Bicycles → Bicycle stores',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''evolution cy'') or contains(lower(description), ''99 bikes'') or contains(lower(description), ''bike barn'') or contains(lower(description), ''cycle''))',
       (SELECT id FROM categories WHERE name = 'Bicycle stores, rentals, and repairs'),
       0, 1, 23, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Bicycles → Bicycle stores');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Airlines → Air transport',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''jetstar'') or contains(lower(description), ''air nz'') or contains(lower(description), ''airnz'') or contains(lower(description), ''qantas'') or contains(lower(description), ''emirates'') or contains(lower(description), ''singapore ai''))',
       (SELECT id FROM categories WHERE name = 'Air transport services'),
       0, 1, 24, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Airlines → Air transport');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Accommodation → Hotels and motels',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''airbnb'') or contains(lower(description), ''booking.com'') or contains(lower(description), ''hotel'') or contains(lower(description), ''motel'') or contains(lower(description), ''holiday park'') or contains(lower(description), ''yha '') or contains(lower(description), ''hostel''))',
       (SELECT id FROM categories WHERE name = 'Hotels, motels, and other temporary accommodation'),
       0, 1, 25, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Accommodation → Hotels and motels');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Supermarkets → Supermarkets and grocery stores',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''pak n save'') or contains(lower(description), ''pakn save'') or contains(lower(description), ''paknsave'') or contains(lower(description), "pak''nsave") or contains(lower(description), ''countdown'') or contains(lower(description), ''new world'') or contains(lower(description), ''woolworths'') or contains(lower(description), ''wwnz'') or contains(lower(description), ''supervalue'') or contains(lower(description), ''super value'') or contains(lower(description), ''four square'') or contains(lower(description), ''freshchoice'') or contains(lower(description), ''fresh choice'') or contains(lower(description), "night ''n da") or contains(lower(description), ''veggie boys'') or contains(lower(description), ''belair super'') or contains(lower(description), ''superette'') or contains(lower(description), ''k road groce'') or contains(lower(description), ''coles '') or contains(lower(description), ''aldi ''))',
       (SELECT id FROM categories WHERE name = 'Supermarkets and grocery stores'),
       0, 1, 26, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Supermarkets → Supermarkets and grocery stores');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Wholefood and bulk stores → Specialty food',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''bin inn'') or contains(lower(description), ''piko wholefo'') or contains(lower(description), ''untamed eart'') or contains(lower(description), ''the butchers'') or contains(lower(description), ''harbour co-o'') or contains(lower(description), ''cheese'') or contains(lower(description), ''cheeser'') or contains(lower(description), ''butcher'') or contains(lower(description), ''farro'') or contains(lower(description), ''moore wilson'') or contains(lower(description), ''the wild bun'') or contains(lower(description), ''sunhill fres'') or contains(lower(description), ''bealey fresh'') or contains(lower(description), ''berrymans''))',
       (SELECT id FROM categories WHERE name = 'Specialty food stores'),
       0, 1, 27, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Wholefood and bulk stores → Specialty food');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Bakeries → Bakeries',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''bakery'') or contains(lower(description), ''bakehouse'') or contains(lower(description), ''bakers delig'') or contains(lower(description), ''breadtop'') or contains(lower(description), ''bakers galle'') or contains(lower(description), ''bread & butt'') or contains(lower(description), ''patisserie'') or contains(lower(description), ''boulangerie'') or contains(lower(description), ''bellbird bak''))',
       (SELECT id FROM categories WHERE name = 'Bakeries'),
       0, 1, 28, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Bakeries → Bakeries');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Meal kits → Meal kit stores',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''wonkybox'') or contains(lower(description), ''hellofresh'') or contains(lower(description), ''my food bag'') or contains(lower(description), ''woop'') or contains(lower(description), ''hello food''))',
       (SELECT id FROM categories WHERE name = 'Meal kit stores'),
       0, 1, 29, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Meal kits → Meal kit stores');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Convenience stores → Convenience stores',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''convenience'') or contains(lower(description), ''munchy mart'') or contains(lower(description), ''c store'') or contains(lower(description), ''dairy'') or contains(lower(description), ''vending''))',
       (SELECT id FROM categories WHERE name = 'Convenience stores'),
       0, 1, 30, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Convenience stores → Convenience stores');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Fast food → Fast food stores',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''mcdonalds'') or contains(lower(description), ''burger king'') or contains(lower(description), ''burgerfuel'') or contains(lower(description), ''burger fuel'') or contains(lower(description), ''dominos'') or contains(lower(description), ''hell pizza'') or contains(lower(description), ''better burge'') or contains(lower(description), ''kfc'') or contains(lower(description), ''subway'') or contains(lower(description), ''wendy'') or contains(lower(description), "carl''s jr") or contains(lower(description), ''pizza hut'') or contains(lower(description), ''uber* eats'') or contains(lower(description), ''uber *eats'') or contains(lower(description), ''ubereats'') or contains(lower(description), ''doordash'') or contains(lower(description), ''dd *doordash'') or contains(lower(description), ''menulog'') or contains(lower(description), ''delivereasy'') or contains(lower(description), "st pierre''s") or contains(lower(description), ''sals pizza'') or contains(lower(description), ''nandos'') or contains(lower(description), ''krispy kreme''))',
       (SELECT id FROM categories WHERE name = 'Fast food stores'),
       0, 1, 31, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Fast food → Fast food stores');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Bars and pubs → Bars, pubs, nightclubs',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''bar & '') or contains(lower(description), ''& bar'') or contains(lower(description), "''s bar") or contains(lower(description), ''tavern'') or contains(lower(description), ''brewery'') or contains(lower(description), ''brewing'') or contains(lower(description), ''hotel bar'') or contains(lower(description), ''pub ''))',
       (SELECT id FROM categories WHERE name = 'Bars, pubs, nightclubs'),
       0, 1, 32, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Bars and pubs → Bars, pubs, nightclubs');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Liquor stores → Liquor stores',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''liquorland'') or contains(lower(description), ''super liquor'') or contains(lower(description), ''bws '') or contains(lower(description), ''black bull'') or contains(lower(description), ''liquor''))',
       (SELECT id FROM categories WHERE name = 'Liquor stores'),
       0, 1, 33, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Liquor stores → Liquor stores');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Cafes and restaurants → Cafes and restaurants',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''cafe'') or contains(lower(description), ''caf '') or contains(lower(description), ''coffee'') or contains(lower(description), ''espresso'') or contains(lower(description), ''sushi'') or contains(lower(description), ''ramen'') or contains(lower(description), ''thai'') or contains(lower(description), ''pizza'') or contains(lower(description), ''burger'') or contains(lower(description), ''kebab'') or contains(lower(description), ''restaurant'') or contains(lower(description), ''eatery'') or contains(lower(description), ''kitchen'') or contains(lower(description), ''bistro'') or contains(lower(description), ''grill'') or contains(lower(description), ''dumpling'') or contains(lower(description), ''noodle'') or contains(lower(description), ''chatime'') or contains(lower(description), ''bubble tea'') or contains(lower(description), ''poke'') or contains(lower(description), ''curry'') or contains(lower(description), ''tandoor'') or contains(lower(description), ''pho '') or contains(lower(description), ''yum cha'') or contains(lower(description), ''roasters'') or contains(lower(description), ''gelato'') or contains(lower(description), ''ice cream'') or contains(lower(description), ''donut'') or contains(lower(description), ''waffle'') or contains(lower(description), ''creamery''))',
       (SELECT id FROM categories WHERE name = 'Cafes and restaurants'),
       0, 1, 34, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Cafes and restaurants → Cafes and restaurants');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Hardware and building supplies → Building supplies',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''bunnings'') or contains(lower(description), ''mitre 10'') or contains(lower(description), ''placemakers'') or contains(lower(description), ''itm '') or contains(lower(description), ''resene'') or contains(lower(description), ''guthrie bowr''))',
       (SELECT id FROM categories WHERE name = 'Building supplies'),
       0, 1, 35, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Hardware and building supplies → Building supplies');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Electronics retailers → Electronic and appliance stores',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''pb tech'') or contains(lower(description), ''pb technolog'') or contains(lower(description), ''computer lou'') or contains(lower(description), ''computerlou'') or contains(lower(description), ''jb hi-fi'') or contains(lower(description), ''noel leeming'') or contains(lower(description), ''harvey norma'') or contains(lower(description), ''newegg'') or contains(lower(description), ''3d printer'') or contains(lower(description), ''3dprinterst'') or contains(lower(description), ''rs componen'') or contains(lower(description), ''element14'') or contains(lower(description), ''digikey'') or contains(lower(description), ''mouser'') or contains(lower(description), ''jaycar''))',
       (SELECT id FROM categories WHERE name = 'Electronic and appliance stores'),
       0, 1, 36, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Electronics retailers → Electronic and appliance stores');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Stationery and office → Stationery and office supplies',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''warehouse stationery'') or contains(lower(description), ''wsl '') or contains(lower(description), ''paper plus'') or contains(lower(description), ''whitcoulls'') or contains(lower(description), ''officemax''))',
       (SELECT id FROM categories WHERE name = 'Stationery and office supplies'),
       0, 1, 37, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Stationery and office → Stationery and office supplies');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'General retail → General retail stores',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''the warehouse'') or contains(lower(description), ''kmart'') or contains(lower(description), ''briscoes'') or contains(lower(description), ''farmers'') or contains(lower(description), ''daiso'') or contains(lower(description), ''3 dollar'') or contains(lower(description), ''3 dollar jap'') or contains(lower(description), ''typo '') or contains(lower(description), ''postie'') or contains(lower(description), ''trade me'') or contains(lower(description), ''aliexpress'') or contains(lower(description), ''amzn mktp'') or contains(lower(description), ''amazon marke'') or contains(lower(description), ''amazon au'') or contains(lower(description), ''temu'') or contains(lower(description), ''ebay'') or contains(lower(description), ''catch.co.nz'') or contains(lower(description), ''mightyape'') or contains(lower(description), ''mighty ape''))',
       (SELECT id FROM categories WHERE name = 'General retail stores'),
       0, 1, 38, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'General retail → General retail stores');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Homeware and furnishing → Home furnishing',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''spotlight'') or contains(lower(description), ''freedom furn'') or contains(lower(description), ''nood '') or contains(lower(description), ''ikea'') or contains(lower(description), ''bed bath''))',
       (SELECT id FROM categories WHERE name = 'Home furnishing and repair stores'),
       0, 1, 39, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Homeware and furnishing → Home furnishing');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Insurance → Insurance',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''insurance'') or contains(lower(description), ''ami insuranc'') or contains(lower(description), ''aa insurance'') or contains(lower(description), ''tower insura'') or contains(lower(description), ''state insura''))',
       (SELECT id FROM categories WHERE name = 'Insurance'),
       0, 1, 40, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Insurance → Insurance');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Pets and vets → Pets',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''vet clinic'') or contains(lower(description), ''at the vets'') or contains(lower(description), ''vets'') or contains(lower(description), ''pet central'') or contains(lower(description), ''pet.kiwi'') or contains(lower(description), ''animates'') or contains(lower(description), ''bird barn'') or contains(lower(description), ''petstock''))',
       (SELECT id FROM categories WHERE name = 'Pets and related supplies, accommodation, and services'),
       0, 1, 41, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Pets and vets → Pets');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Laundromats → Laundry',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''laundromat'') or contains(lower(description), ''laundry'') or contains(lower(description), ''drycleaning'') or contains(lower(description), ''dry cleaning''))',
       (SELECT id FROM categories WHERE name = 'Laundry and drycleaning'),
       0, 1, 42, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Laundromats → Laundry');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Waste services → Waste and recycling',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''waste manage'') or contains(lower(description), ''green gorill'') or contains(lower(description), ''skip bin''))',
       (SELECT id FROM categories WHERE name = 'Waste and recycling services'),
       0, 1, 43, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Waste services → Waste and recycling');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Pharmacies → Pharmacies',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''unichem'') or contains(lower(description), ''pharmacy'') or contains(lower(description), ''pharm'') or contains(lower(description), ''chemist'') or contains(lower(description), ''life pharmac''))',
       (SELECT id FROM categories WHERE name = 'Pharmacies'),
       0, 1, 44, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Pharmacies → Pharmacies');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Doctors, dentists, medical → Doctors and physicians',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''medical cent'') or contains(lower(description), ''dental'') or contains(lower(description), ''doctors'') or contains(lower(description), ''hospital'') or contains(lower(description), ''radiology'') or contains(lower(description), ''physio'') or contains(lower(description), ''optometr'') or contains(lower(description), ''healthcare''))',
       (SELECT id FROM categories WHERE name = 'Doctors and physicians'),
       0, 1, 45, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Doctors, dentists, medical → Doctors and physicians');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Gyms and recreation → Gyms and fitness',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''parakiore'') or contains(lower(description), ''les mills'') or contains(lower(description), ''cityfitness'') or contains(lower(description), ''jetts'') or contains(lower(description), ''anytime fitn'') or contains(lower(description), ''recreation'') or contains(lower(description), ''aquatic'') or contains(lower(description), ''swim''))',
       (SELECT id FROM categories WHERE name = 'Gyms, fitness, aquatic facilities, yoga, pilates'),
       0, 1, 46, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Gyms and recreation → Gyms and fitness');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Op shops → Secondhand and opportunity stores',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''op shop'') or contains(lower(description), '' ops '') or contains(lower(description), ''secondhand'') or contains(lower(description), ''savemart'') or contains(lower(description), ''save mart'') or contains(lower(description), ''city mission'') or contains(lower(description), ''hospice shop'') or contains(lower(description), ''hospice''))',
       (SELECT id FROM categories WHERE name = 'Secondhand and opportunity stores'),
       0, 1, 47, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Op shops → Secondhand and opportunity stores');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Barbers and salons → Haircuts and treatments',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''barber'') or contains(lower(description), ''hair '') or contains(lower(description), ''salon'') or contains(lower(description), ''haircut''))',
       (SELECT id FROM categories WHERE name = 'Haircuts and treatments'),
       0, 1, 48, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Barbers and salons → Haircuts and treatments');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Clothing → Clothing stores',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''as colour'') or contains(lower(description), ''cotton on'') or contains(lower(description), ''hallenstein'') or contains(lower(description), ''uniqlo'') or contains(lower(description), ''bendon'') or contains(lower(description), ''glassons'') or contains(lower(description), ''ballantynes'') or contains(lower(description), ''barkers'') or contains(lower(description), ''kathmandu'') or contains(lower(description), ''macpac'') or contains(lower(description), ''icebreaker'') or contains(lower(description), ''h&m'') or contains(lower(description), ''zara''))',
       (SELECT id FROM categories WHERE name = 'Clothing stores'),
       0, 1, 49, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Clothing → Clothing stores');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Shoes → Shoe stores',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''shoe'') or contains(lower(description), ''hannahs'') or contains(lower(description), ''number one s'') or contains(lower(description), ''platypus''))',
       (SELECT id FROM categories WHERE name = 'Shoe stores'),
       0, 1, 50, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Shoes → Shoe stores');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Craft and fabric → Fabric and sewing',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''knit world'') or contains(lower(description), ''knit '') or contains(lower(description), ''spotlight'') or contains(lower(description), ''bernina'') or contains(lower(description), ''wool ''))',
       (SELECT id FROM categories WHERE name = 'Fabric, sewing, knitting, and related supplies'),
       0, 1, 51, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Craft and fabric → Fabric and sewing');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Universities → Tertiary education',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''uni of auckl'') or contains(lower(description), ''university o'') or contains(lower(description), ''ak uni'') or contains(lower(description), ''academic dre'') or contains(lower(description), ''uoa'') or contains(lower(description), ''canterbury u'') or contains(lower(description), ''language tra''))',
       (SELECT id FROM categories WHERE name = 'Universities, professional schools, and other tertiary education'),
       0, 1, 52, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Universities → Tertiary education');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Book stores → Book stores',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''bookshop'') or contains(lower(description), ''book store'') or contains(lower(description), ''bookstore'') or contains(lower(description), ''bookbarn'') or contains(lower(description), ''kinokuniya'') or contains(lower(description), ''unity books''))',
       (SELECT id FROM categories WHERE name = 'Book stores'),
       0, 1, 53, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Book stores → Book stores');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Libraries → Libraries',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''library'') or contains(lower(description), ''libraries''))',
       (SELECT id FROM categories WHERE name = 'Libraries'),
       0, 1, 54, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Libraries → Libraries');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Charities and donations → Welfare and charity',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''red cross'') or contains(lower(description), ''red cros'') or contains(lower(description), ''salvation ar'') or contains(lower(description), ''st john'') or contains(lower(description), ''auckland cit'') or contains(lower(description), ''unicef'') or contains(lower(description), ''greenpeace'') or contains(lower(description), ''world vision'') or contains(lower(description), ''donation''))',
       (SELECT id FROM categories WHERE name = 'Welfare and charity'),
       0, 1, 55, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Charities and donations → Welfare and charity');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Cinemas → Cinemas',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''cinema'') or contains(lower(description), ''hoyts'') or contains(lower(description), ''event cinema'') or contains(lower(description), ''reading cine''))',
       (SELECT id FROM categories WHERE name = 'Cinemas'),
       0, 1, 56, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Cinemas → Cinemas');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Florists → Floral supplies',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''florist'') or contains(lower(description), ''flowers'') or contains(lower(description), ''floral''))',
       (SELECT id FROM categories WHERE name = 'Floral supplies and services'),
       0, 1, 57, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Florists → Floral supplies');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Sports and outdoors → Sports equipment',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''torpedo 7'') or contains(lower(description), ''torpedo7'') or contains(lower(description), ''rebel sport'') or contains(lower(description), ''hunting & fi'') or contains(lower(description), ''outdoor'') or contains(lower(description), ''sports''))',
       (SELECT id FROM categories WHERE name = 'Sports equipment and supplies'),
       0, 1, 58, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Sports and outdoors → Sports equipment');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Massage and spa → Cosmetic and spa services',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''massage'') or contains(lower(description), ''beauty'') or contains(lower(description), ''spa ''))',
       (SELECT id FROM categories WHERE name = 'Cosmetic, health spas, and relaxation massage services'),
       0, 1, 59, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Massage and spa → Cosmetic and spa services');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Events and attractions → Events and tickets',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''eventbrite'') or contains(lower(description), ''ticketek'') or contains(lower(description), ''iticket'') or contains(lower(description), ''museum'') or contains(lower(description), ''zoo '') or contains(lower(description), ''skydiving'') or contains(lower(description), ''paradice'') or contains(lower(description), ''ice skating'') or contains(lower(description), ''conservati''))',
       (SELECT id FROM categories WHERE name = 'Events and tickets (not elsewhere classified)'),
       0, 1, 60, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Events and attractions → Events and tickets');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Gift and souvenir stores → Gift and souvenir',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''gift'') or contains(lower(description), ''souvenir''))',
       (SELECT id FROM categories WHERE name = 'Gift and souvenir stores'),
       0, 1, 61, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Gift and souvenir stores → Gift and souvenir');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Brokers and exchanges → Financial asset brokers',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''sharesies'') or contains(lower(description), ''hatch invest'') or contains(lower(description), ''easy crypto'') or contains(lower(description), ''interactive b'') or contains(lower(description), ''binance'') or contains(lower(description), ''kernel wealt'') or contains(lower(description), ''simplicity''))',
       (SELECT id FROM categories WHERE name = 'Financial asset brokers, exchanges, and managed funds'),
       0, 1, 62, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Brokers and exchanges → Financial asset brokers');

INSERT INTO rules (name, description, expression, set_category_id,
                   overwrite_manual, stop_on_match, priority, enabled)
SELECT 'Internal bank transfers → Transfer',
       'Default rule shipped with Sure.',
       '(contains(lower(description), ''mb transfer'') or contains(lower(description), ''fn transfer'') or contains(lower(description), ''transfer to'') or contains(lower(description), ''transfer fro'') or contains(lower(description), ''transfer cor'') or contains(lower(description), ''bill payment to'') or contains(lower(description), ''pmt to fc'') or startsWith(lower(description), ''fc0'') or startsWith(lower(description), ''fc1'') or startsWith(lower(description), ''fc2'') or startsWith(lower(description), ''fc3'') or startsWith(lower(description), ''fc4'') or startsWith(lower(description), ''fc5'') or startsWith(lower(description), ''fc6'') or startsWith(lower(description), ''fc7'') or startsWith(lower(description), ''fc8'') or startsWith(lower(description), ''fc9'') or startsWith(lower(description), ''tfr to'') or startsWith(lower(description), ''tfr from''))',
       (SELECT id FROM categories WHERE name = 'Transfer'),
       0, 1, 63, 1
WHERE NOT EXISTS (SELECT 1 FROM rules WHERE name = 'Internal bank transfers → Transfer');
