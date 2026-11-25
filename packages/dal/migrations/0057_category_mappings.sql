-- Source labels are configuration, separate from the user's category tree.
CREATE TABLE category_mappings (
    id INTEGER PRIMARY KEY,
    source TEXT NOT NULL COLLATE NOCASE,
    source_group TEXT NOT NULL COLLATE NOCASE DEFAULT '',
    source_name TEXT NOT NULL COLLATE NOCASE,
    category_id INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE(source, source_group, source_name)
) STRICT;
CREATE INDEX idx_category_mappings_category ON category_mappings(category_id);

-- A resolved fallback belongs to this import, not to every future mapping edit.
ALTER TABLE transactions ADD COLUMN pending_category_id INTEGER REFERENCES categories(id) ON DELETE SET NULL;
CREATE INDEX idx_transactions_pending_category ON transactions(pending_category_id) WHERE pending_category_id IS NOT NULL;

-- Seed Akahu's expense labels once. Prefer an existing original category; after a
-- consolidation, use the corresponding summary category if present. No categories
-- or transaction assignments are created or changed by this migration.
WITH defaults(source_group, source_name, summary_name) AS (VALUES
    ('', 'Appearance', 'Clothing'),
    ('', 'Education', 'Education & books'),
    ('', 'Food', 'Groceries'),
    ('', 'Health', 'Health'),
    ('', 'Household', 'Home & household'),
    ('', 'Housing', 'Housing'),
    ('', 'Lifestyle', 'Entertainment & hobbies'),
    ('', 'Professional Services', 'Professional services'),
    ('', 'Transport', 'Transport'),
    ('', 'Utilities', 'Utilities & insurance'),
    ('Lifestyle', 'Air transport services', 'Travel'),
    ('Transport', 'Automotive parts and accessories', 'Transport'),
    ('Transport', 'Automotive repair and servicing', 'Transport'),
    ('Food', 'Bakeries', 'Eating out'),
    ('Professional Services', 'Bank fees and charges', 'Interest & fees'),
    ('Lifestyle', 'Bars, pubs, nightclubs', 'Alcohol'),
    ('Transport', 'Bicycle stores, rentals, and repairs', 'Transport'),
    ('Education', 'Book stores', 'Education & books'),
    ('Household', 'Building supplies', 'Home & household'),
    ('Transport', 'Bus and shuttle transport services', 'Transport'),
    ('Professional Services', 'Business software and cloud services', 'Professional services'),
    ('Lifestyle', 'Cafes and restaurants', 'Eating out'),
    ('Lifestyle', 'Cinemas', 'Entertainment & hobbies'),
    ('Appearance', 'Clothing stores', 'Clothing'),
    ('Food', 'Convenience stores', 'Groceries'),
    ('Lifestyle', 'Cosmetic, health spas, and relaxation massage services', 'Personal care'),
    ('Lifestyle', 'Digital gaming products and services', 'Entertainment & hobbies'),
    ('Health', 'Doctors and physicians', 'Health'),
    ('Utilities', 'Electricity services', 'Utilities & insurance'),
    ('Household', 'Electronic and appliance stores', 'Home & household'),
    ('Lifestyle', 'Events and tickets (not elsewhere classified)', 'Entertainment & hobbies'),
    ('Appearance', 'Fabric, sewing, knitting, and related supplies', 'Entertainment & hobbies'),
    ('Lifestyle', 'Fast food stores', 'Eating out'),
    ('Professional Services', 'Financial asset brokers, exchanges, and managed funds', NULL),
    ('Lifestyle', 'Floral supplies and services', 'Gifts & giving'),
    ('Transport', 'Fuel stations', 'Transport'),
    ('Household', 'General retail stores', 'Home & household'),
    ('Lifestyle', 'Gift and souvenir stores', 'Gifts & giving'),
    ('Health', 'Gyms, fitness, aquatic facilities, yoga, pilates', 'Fitness'),
    ('Appearance', 'Haircuts and treatments', 'Personal care'),
    ('Household', 'Home furnishing and repair stores', 'Home & household'),
    ('Lifestyle', 'Hotels, motels, and other temporary accommodation', 'Travel'),
    ('Household', 'Insurance', 'Utilities & insurance'),
    ('', 'Interest charged', 'Interest & fees'),
    ('Utilities', 'Internet services', 'Utilities & insurance'),
    ('Household', 'Laundry and drycleaning', 'Home & household'),
    ('Education', 'Libraries', 'Education & books'),
    ('Lifestyle', 'Liquor stores', 'Alcohol'),
    ('Housing', 'Local government', 'Housing'),
    ('Food', 'Meal kit stores', 'Groceries'),
    ('Lifestyle', 'Media and entertainment streaming services', 'Subscriptions'),
    ('Transport', 'Parking services', 'Transport'),
    ('Household', 'Pets and related supplies, accommodation, and services', 'Pets'),
    ('Health', 'Pharmacies', 'Health'),
    ('Appearance', 'Secondhand and opportunity stores', 'Home & household'),
    ('Appearance', 'Shoe stores', 'Clothing'),
    ('Food', 'Specialty food stores', 'Groceries'),
    ('Lifestyle', 'Sports equipment and supplies', 'Entertainment & hobbies'),
    ('Household', 'Stationery and office supplies', 'Home & household'),
    ('Food', 'Supermarkets and grocery stores', 'Groceries'),
    ('Professional Services', 'Tax payments', 'Taxes'),
    ('Transport', 'Taxi, rideshare, and on-demand transport services', 'Transport'),
    ('Utilities', 'Telecommunication services', 'Utilities & insurance'),
    ('Transport', 'Transport services (not elsewhere classified)', 'Transport'),
    ('Education', 'Universities, professional schools, and other tertiary education', 'Education & books'),
    ('Household', 'Waste and recycling services', 'Utilities & insurance'),
    ('Utilities', 'Water services', 'Utilities & insurance'),
    ('Lifestyle', 'Welfare and charity', 'Gifts & giving'),
    ('Food', 'Ice cream, gelato, nut, and confectionary stores', 'Eating out'),
    ('Lifestyle', 'Wineries, breweries, and distilleries', 'Alcohol'),
    ('Household', 'Variety stores', 'Home & household'),
    ('Household', 'Painting supplies and services', 'Home & household'),
    ('Professional Services', 'National government services', 'Professional services'),
    ('', 'Subscriptions', 'Subscriptions'),
    ('Professional Services', 'Postal services', 'Professional services'),
    ('Health', 'Veterinary services', 'Pets'),
    ('Lifestyle', 'Airports', 'Travel'),
    ('Professional Services', 'Membership organisations (not elsewhere classified)', 'Subscriptions'),
    ('Transport', 'Car and motorcycle rentals', 'Travel')
)
INSERT INTO category_mappings(source,source_group,source_name,category_id)
SELECT 'akahu', d.source_group, d.source_name,
       CASE WHEN d.summary_name IS NULL THEN NULL ELSE COALESCE(
           (SELECT c.id FROM categories c LEFT JOIN categories p ON p.id=c.parent_id
             WHERE c.name=d.source_name COLLATE NOCASE AND c.kind='expense'
               AND COALESCE(p.name,'')=d.source_group COLLATE NOCASE LIMIT 1),
           (SELECT c.id FROM categories c WHERE c.name=d.summary_name COLLATE NOCASE
               AND c.kind='expense' AND c.parent_id IS NULL LIMIT 1)
       ) END
  FROM defaults d;
